use super::*;
use alloc::sync::Arc;

use crate::test_utils::{find_event, init_mock_pipe, next_publish, next_publish_namespace, next_request, next_subscribe, subscribe_response, subscribed_track};
use moq_wire::{MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_16};


/// Resets written-but-unsent streams, leaves delivered ones alone.
#[test]
fn cancel_unfinished_streams_resets_written_but_undelivered() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    // Delivered: written and flushed to the peer, FIN included.
    let delivered = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { end_of_group: true, properties: Some(&kvps), ..OutgoingObject::new(delivered, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, delivered, b"abc", &mut p.server_t).unwrap();
    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    p.advance();
    assert!(p.server_t.stream_finished(3), "first object must count as fully delivered before the test proper");

    // Written in full, FIN included, but not yet sent.
    let queued = Location { group: 1, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { end_of_group: true, properties: Some(&kvps), ..OutgoingObject::new(queued, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, queued, b"xyz", &mut p.server_t).unwrap();
    p.server.fin_send_subgroup(track_alias, 1, 0, &mut p.server_t);

    p.server.cancel_unfinished_streams(track_alias, ResetCode::Other(0x4321), &mut p.server_t);
    p.advance();

    // stream 7: server's second uni stream (queued object)
    let mut buf = [0u8; 16];
    let raw = p.client_t.recv_stream(7, &mut buf);
    assert!(matches!(raw, Err(StreamError::StreamReset(0x4321))), "queued object must be reset, got {raw:?}");
    match p.client.recv_new_obj_hdr(track_alias, &mut p.client_t).unwrap() {
        RecvObjHdr::Header { location, .. } => assert_eq!(location, delivered),
        other => panic!("the delivered object must still be readable, got {other:?}"),
    }
}

/// The Subgroup header carries the object's publisher priority, 128 unless given (draft-ietf-moq-transport-18 §12.4).
#[test]
fn publisher_priority_reaches_the_subscriber() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    for (subgroup_id, object, expected) in [(0, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 1), 128), (1, OutgoingObject { publisher_priority: 7, ..OutgoingObject::new(Location { group: 0, object: 1 }, 1, 1) }, 7)] {
        let location = object.location;
        p.server.send_obj_hdr(track_alias, object, &mut p.server_t).unwrap();
        p.server.send_obj_pld(track_alias, location, b"x", &mut p.server_t).unwrap();
        p.advance();
        let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
        assert!(matches!(event, Ok(RecvObjHdr::Header { subgroup_id: s, publisher_priority, .. }) if s == subgroup_id && publisher_priority == expected), "{event:?}");
        let mut buf = [0u8; 1];
        p.client.recv_obj_pld(track_alias, location, &mut buf, &mut p.client_t).unwrap();
    }
}

/// An object without payload carries its Object Status (draft-ietf-moq-transport-18 §11.2.1.1).
#[test]
fn object_status_reaches_the_subscriber() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let end = Location { group: 0, object: 3 };
    let status = moq_wire::object::ObjectStatus::EndOfGroup;
    p.server.send_obj_hdr(track_alias, OutgoingObject { status, ..OutgoingObject::new(end, 0, 0) }, &mut p.server_t).unwrap();
    p.advance();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Header { location, ref header, .. }) if location == end && header.status() == Some(status)), "{event:?}");
}

/// An undefined Object Status is a PROTOCOL_VIOLATION (draft-ietf-moq-transport-18 §11.2.1.1).
#[test]
fn undefined_object_status_closes_the_session() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    assert!(track_alias.0 < 64, "single byte varint");
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // header | object id, properties length, payload length 0, status
    p.server_t.send_stream(stream_id, &[0x15, track_alias.0 as u8, 0, 0, 0, 0, 0, 0, 0x2], false).unwrap();
    p.advance();
    assert!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t).is_err());
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// An Object sent in a datagram arrives whole (draft-ietf-moq-transport-18 §11.3).
#[rstest::rstest]
fn datagram_object_reaches_the_subscriber(#[values(&b"abc"[..], &b""[..])] sent: &[u8]) {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();
    let location = Location { group: 2, object: 4 };
    let object = OutgoingDatagram { location, publisher_priority: 7, end_of_group: true, status: ObjectStatus::Normal, properties: &kvps, received_at: None, payload: sent };
    p.server.send_datagram_obj(track_alias, object, &mut p.server_t).unwrap();
    p.advance();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Datagram { location: l, publisher_priority: 7, end_of_group: true, ref payload, .. })
        if l == location && payload == sent), "{event:?}");
}

/// A datagram without a Publisher Priority inherits the track's DEFAULT PUBLISHER PRIORITY
/// (draft-ietf-moq-transport-18 §12.4).
#[test]
fn datagram_inherits_the_default_publisher_priority() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let properties = KeyValuePairs::new().with(KeyValuePair::Varint(moq_wire::property::DEFAULT_PUBLISHER_PRIORITY, 7));
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), properties, true, None, &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    let request = next_publish(&mut p.client);
    let _subscription = p.client.accept_request(request, crate::PublishOk::default(), &mut p.client_t).unwrap();
    let datagram = ObjectDatagram { track_alias, location: Location { group: 0, object: 1 }, publisher_priority: None, end_of_group: false, properties: KeyValuePairs::new(), status: None };
    p.server_t.send_datagram(&datagram.encode(b"x", MOQ_VERSION_DRAFT_16).unwrap()).unwrap();
    p.advance();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Datagram { publisher_priority: 7, .. })), "{event:?}");
}

/// A datagram arriving before the PUBLISH of its track waits for it (draft-ietf-moq-transport-18 §11.3).
#[test]
fn datagram_before_its_track_waits_for_it() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    let datagram = ObjectDatagram { track_alias, location: Location { group: 0, object: 1 }, publisher_priority: Some(9), end_of_group: false, properties: KeyValuePairs::new(), status: None };
    p.server_t.send_datagram(&datagram.encode(b"x", MOQ_VERSION_DRAFT_16).unwrap()).unwrap();
    // the datagram first, the PUBLISH not yet
    p.deliver();
    p.client.handle(&mut p.client_t, &[], p.now);
    assert!(p.client.pending_datagrams.contains_key(&track_alias));
    p.advance();
    let request = next_publish(&mut p.client);
    let _subscription = p.client.accept_request(request, crate::PublishOk::default(), &mut p.client_t).unwrap();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Datagram { ref payload, .. }) if payload == b"x"), "{event:?}");
}

/// draft-ietf-moq-transport-18 §11.3.1: an invalid Type is a PROTOCOL_VIOLATION
#[test]
fn malformed_datagram_closes_the_session() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    subscribed_track(&mut p);
    p.server_t.send_datagram(&[0x22, 0, 0, 0, 128, 3]).unwrap();
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// Out of tokens, padding requests a wakeup via `timeout()`.
#[test]
fn rate_limited_padding_requests_wakeup() {
    let mut config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    // 1200-byte packet every 4.8ms.
    config.padding_stream_max_bytes_per_sec = Some(250_000);
    let mut p = init_mock_pipe(config);
    assert_eq!(p.client.timeout_at(), None, "nothing to wake for before padding starts");

    p.client.start_padding_stream();
    p.advance(); // runs the padding stream: the bucket starts empty

    let at = p.client.timeout_at().expect("a padding stream waiting on tokens must request a wakeup");
    assert!(at <= p.now + Duration::from_micros(4800), "next full packet is due within 4.8ms, got {at:?}");
}

/// A subgroup stream reset between objects is reported as reset, not as
/// finished: more objects may exist (draft-16 §10.4.3).
#[test]
fn recv_reports_subgroup_reset() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, 0, 3), &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    let mut buf = [0u8; 8];
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert!(matches!(p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t), Ok(3)));

    p.server.reset_send_subgroup(track_alias, 0, 0, ResetCode::Canceled, &mut p.server_t);
    p.advance();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::SubgroupReset { location, .. }) if location == loc), "{event:?}");
}

/// Request IDs are shared by all request types: a PUBLISH_NAMESPACE after
/// a SUBSCRIBE is valid, a skipped ID is not.
#[test]
fn peer_request_ids_are_consecutive_across_request_types() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    subscribed_track(&mut p);

    let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();

    let request_id = announcement.request_id();
    p.advance();
    assert!(!p.server.closed);
    assert!(p.server.unanswered_requests.contains_key(&request_id));

    assert!(p.server.accept_peer_request_id(request_id.next().next()).is_err(), "skipped request id");
}

/// Draft 12+: the publisher assigns the alias, a pending SUBSCRIBE has
/// none (a REQUEST_ERROR must not drop a track with alias == request id).
#[test]
fn pending_subscribe_has_no_alias_from_draft_12() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t)
        .unwrap();
    let request_id = subscription.request_id();
    assert_eq!(p.client.pending_subscribe[&request_id].track_alias(), None);
}

/// A REQUEST_ERROR for a draft 12+ SUBSCRIBE must not touch a track whose
/// alias happens to equal the request id.
#[test]
fn request_error_leaves_unrelated_track_alone() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t)
        .unwrap();
    let request_id = subscription.request_id();
    let alias = TrackAlias(request_id.0);
    p.client.in_tracks.insert(alias, InTrack::new(alias, moq_wire::DEFAULT_PUBLISHER_PRIORITY));
    p.advance();
    let request = next_subscribe(&mut p.server);
    p.server.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.server_t).unwrap();
    p.advance();
    assert!(matches!(subscribe_response(&mut p.client, request_id), Some(Err(_))));
    assert!(p.client.in_tracks.contains_key(&alias));
}

/// Streams the publisher opens after an UNSUBSCRIBE are stopped too.
#[test]
fn late_stream_of_dropped_track_is_dropped() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, 0, 3), &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    assert!(p.client.in_streams.is_empty());
    assert!(p.client.pending_streams.is_empty(), "not parked waiting for a track");
    assert_eq!(p.client.dropped_in_streams.len(), 1);
}

/// Draft 11: the subscriber chose the alias, so unsubscribing before
/// SUBSCRIBE_OK still drops that track's streams.
#[test]
fn unsubscribe_before_subscribe_ok_drops_known_alias() {
    let config = Config { version_negotiation: VersionNegotiation::InBand { candidates: vec![MOQ_VERSION_DRAFT_11] }, ..Config::default() };
    let mut p = init_mock_pipe(config);
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t)
        .unwrap();
    let request_id = subscription.request_id();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(p.client.dropped_aliases.contains_key(&TrackAlias(request_id.0)));
}

/// Drafts 7–10: only ANNOUNCE is exempt from the request limit.
#[test]
fn only_announce_ignores_the_limit_before_draft_11() {
    let config = Config { version_negotiation: VersionNegotiation::InBand { candidates: vec![MOQ_VERSION_DRAFT_10] }, ..Config::default() };
    let mut p = init_mock_pipe(config);
    let first_subscribe_id = p.client.next_request_id;
    p.client.max_request_id = first_subscribe_id;
    let subscribe = p.client.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t);
    assert!(matches!(subscribe, Err(Error::RequestBlocked)));
    assert!(p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).is_ok());
    assert!(p.client.publish_namespace("n2".parse().unwrap(), &mut p.client_t).is_ok());
    // ...and doesn't use up the SUBSCRIBE limit either
    let subscribe = p.client.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t);
    assert!(matches!(subscribe, Err(Error::RequestBlocked)), "still at the limit");
    p.client.max_request_id = p.client.max_request_id.next();
    let subscribe = p.client.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t);
    assert_eq!(subscribe.unwrap().request_id(), first_subscribe_id, "SUBSCRIBE IDs don't skip the ANNOUNCEs");
}

/// A REQUEST_UPDATE with an out of range Forward value closes the session.
#[test]
fn invalid_forward_in_request_update_closes_session() {
    let mut p = d16_pipe();
    subscribed_track(&mut p);
    let subscription = p.subscriptions.pop().unwrap();
    let request_id = subscription.request_id();
    let update_id = p.client.next_request_id;
    p.client.next_request_id = p.client.next_request_id.next();
    // REQUEST_UPDATE with a FORWARD of 2, which the typed field can't hold: type, length, ids, FORWARD 2
    let bytes = [0x02, 0, 5, update_id.0 as u8, request_id.0 as u8, 1, 0x10, 2];
    p.client_t.send_stream(0, &bytes, false).unwrap();
    p.advance();
    assert_eq!(p.server_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// A rejected PUBLISH stops the streams that already arrived for it.
#[test]
fn rejected_publish_drops_its_streams() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    let request = next_publish(&mut p.client);
    p.client.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.client_t).unwrap();
    assert!(p.client.dropped_aliases.contains_key(&track_alias));
}

/// The mock ignores STOP_SENDING like moq_web/moq_tokio: a dropped
/// stream's remaining bytes must be discarded, not parsed as a new stream.
#[test]
fn dropped_stream_is_drained_not_reparsed() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, 0, 6), &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);

    p.server.send_obj_pld(track_alias, loc, b"def", &mut p.server_t).unwrap();
    p.advance();
    assert!(p.client.in_streams.is_empty());
    assert_eq!(p.client.dropped_in_streams.len(), 1);

    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    p.advance();
    assert!(p.client.dropped_in_streams.is_empty(), "forgotten once it ends");
}

#[test]
fn request_ids_over_the_limit_and_out_of_order_are_distinguished() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    subscribed_track(&mut p);
    let limit = p.server.out_max_request_id;
    assert!(matches!(p.server.accept_peer_request_id(limit), Err((SessionCloseCode::TooManyRequests, _))));
    let next = p.server.next_expected_request_id;
    assert!(matches!(p.server.accept_peer_request_id(next.next()), Err((SessionCloseCode::InvalidRequestId, _))));
    assert!(p.server.accept_peer_request_id(next).is_ok());
}

/// From draft 17 requests arrive on their own streams, in any order: only the parity and duplicates are checked.
#[test]
fn draft_17_request_ids_may_arrive_out_of_order() {
    let mut p = d17_pipe();
    let next = p.server.next_expected_request_id;
    let invalid = |r: core::result::Result<(), (SessionCloseCode, String)>| matches!(r, Err((SessionCloseCode::InvalidRequestId, _)));
    assert!(p.server.accept_peer_request_id(next.next()).is_ok());
    assert!(p.server.accept_peer_request_id(next).is_ok());
    assert_eq!(p.server.next_expected_request_id, next.next().next());
    assert!(invalid(p.server.accept_peer_request_id(next)), "duplicate");
    assert!(invalid(p.server.accept_peer_request_id(next.next())), "duplicate");
    assert!(invalid(p.server.accept_peer_request_id(RequestId(next.0 + 5))), "the server's parity");
    assert!(p.server.peer_request_ids_ahead.is_empty());
}

#[test]
fn draft_17_request_ids_ahead_are_bounded() {
    let mut p = d17_pipe();
    let mut ahead = p.server.next_expected_request_id;
    for _ in 0..super::MAX_PEER_REQUEST_IDS_AHEAD {
        ahead = ahead.next();
        assert!(p.server.accept_peer_request_id(ahead).is_ok());
    }
    assert!(matches!(p.server.accept_peer_request_id(ahead.next()), Err((SessionCloseCode::TooManyRequests, _))));
}

/// MAX_REQUEST_ID raises the limit once half of it is used, so a peer
/// can keep subscribing for the whole session.
#[test]
fn request_id_limit_is_raised_before_it_runs_out() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), out_max_request_id: RequestId(4).0, ..Config::default() };
    let mut p = init_mock_pipe(config);
    for _ in 0..10 {
        p.subscriptions.push(p.client.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t).unwrap());
        p.advance();
        p.advance();
    }
    assert_eq!(p.server_t.closed_with, None);
    assert_eq!(p.server.unanswered_requests.len(), 10);
    assert_eq!(p.client.max_request_id, p.server.out_max_request_id);
}

#[test]
fn decreasing_max_request_id_closes_session() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let cm = ControlMessageEnum::MaxRequestId(MaxRequestIdMessage { max_request_id: RequestId(0) });
    p.server.send_control_message(&mut p.server_t, &cm);
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

#[rstest::rstest]
#[case::wrong_role(ControlMessageEnum::ClientSetup(ClientSetupMessage { supported_versions: vec![], options: SetupOptions::default() }))]
#[case::second_setup(ControlMessageEnum::ServerSetup(ServerSetupMessage::new(MOQ_VERSION_DRAFT_16, SetupOptions::default())))]
fn unexpected_setup_closes_session(#[case] setup: ControlMessageEnum) {
    let mut p = d16_pipe();
    p.server.send_control_message(&mut p.server_t, &setup);
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// Streams of an unsubscribed track are stopped, not left unread.
#[test]
fn unsubscribe_drops_track_streams() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, 0, 3), &mut p.server_t).unwrap();
    p.advance();
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert!(!p.client.in_streams.is_empty());

    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(p.client.in_streams.is_empty());
}

#[test]
fn zero_length_object_is_sent_and_received() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    let empty = Location { group: 0, object: 0 };
    let next = Location { group: 0, object: 1 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(empty, 0, 0) }, &mut p.server_t).unwrap();
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(next, 0, 1) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, next, b"x", &mut p.server_t).unwrap();
    p.advance();

    let first = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(first, Ok(RecvObjHdr::Header { location, ref header, .. }) if location == empty && header.payload_len() == 0), "{first:?}");
    let second = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(second, Ok(RecvObjHdr::Header { location, .. }) if location == next), "{second:?}");
}

/// A FIN declares the subgroup complete, it can't continue afterwards.
#[test]
fn send_after_fin_is_rejected() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    let first = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(first, 0, 1) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, first, b"a", &mut p.server_t).unwrap();
    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    let next = p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(Location { group: 0, object: 1 }, 0, 1) }, &mut p.server_t);
    assert!(matches!(next, Err(Error::SubgroupFinished)), "{next:?}");
}

#[test]
fn request_update_for_ended_subscription_is_rejected() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    subscribed_track(&mut p);
    // the publisher ends it while the update is in flight
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);

    let request_id = p.client.request_update_forward(&p.subscriptions[0], true, None, &mut p.client_t).unwrap();
    p.advance();
    assert!(p.server.unanswered_requests.is_empty());
    p.advance();
    assert!(find_event(&mut p.client, |e| matches!(e, Event::RequestFailed { request_id: id, .. } if id == request_id).then_some(())).is_some());
}

/// An object sent between sweeps still bounds `timeout()`.
#[test]
fn timeout_covers_object_sent_after_sweep() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions { delivery_timeout: Some(10_000), ..Default::default() }, &mut p.client_t)
        .unwrap();
    let request_id = subscription.request_id();
    p.advance();
    let request = next_subscribe(&mut p.server);
    let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    subscribe_response(&mut p.client, request_id).unwrap().unwrap();
    assert_eq!(p.server.timeout_at(), None);

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(p.now), ..OutgoingObject::new(loc, 0, 3) }, &mut p.server_t).unwrap();
    assert!(p.server.timeout_at().is_some_and(|at| at <= p.now + Duration::from_secs(10)), "{:?}", p.server.timeout_at());
}

#[test]
fn unknown_data_stream_type_closes_session() {
    let mut p = init_mock_pipe(Config::default());
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // SUBGROUP_ID_MODE 0b11 is reserved
    p.server_t.send_stream(stream_id, &[0x16, 0, 0, 0], false).unwrap();
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

#[test]
fn malformed_object_header_closes_session() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    assert!(track_alias < TrackAlias(64), "single byte varint");
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // header | object id, extensions length 2, odd key 1 with a 5 byte value, payload length, payload
    let raw = [0x15, track_alias.0 as u8, 0, 0, 0, 0, 2, 1, 5, 1, b'x'];
    p.server_t.send_stream(stream_id, &raw, false).unwrap();
    p.advance();

    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Err(Error::Wire(_))), "{event:?}");
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

#[test]
fn ended_control_stream_closes_session() {
    let mut p = init_mock_pipe(Config::default());
    p.server_t.send_stream(0, &[], true).unwrap();
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// Header type 0x13 encodes the Subgroup ID as the first Object ID.
#[test]
fn recv_subgroup_id_from_first_object() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    assert!(track_alias < TrackAlias(64), "single byte varint");

    let stream_id = p.server_t.open_uni_stream().unwrap();
    // type, alias, group, priority, object id, extensions length, payload length, payload
    let raw = [0x13, track_alias.0 as u8, 0, 0, 5, 0, 1, b'x'];
    p.server_t.send_stream(stream_id, &raw, false).unwrap();
    p.advance();

    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Header { subgroup_id: 5, .. })), "{event:?}");
}

/// A resend that arrives only after the reset was read is surfaced as a
/// new header, so the app can restart the object it canceled.
#[test]
fn object_resent_after_reset_is_surfaced_again() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(loc, 0, 6) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    let mut buf = [0u8; 8];
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert!(matches!(p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t), Ok(3)));
    p.server.reset_send_subgroup(track_alias, 0, 0, ResetCode::Canceled, &mut p.server_t);
    p.advance();
    assert!(matches!(p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t), Err(Error::Fin)));

    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(loc, 0, 6) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abcdef", &mut p.server_t).unwrap();
    p.advance();
    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Ok(RecvObjHdr::Header { location, .. }) if location == loc), "{event:?}");
    assert!(matches!(p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t), Ok(6)));
}

/// A subgroup reset mid-object and resent on a new stream: the new copy
/// takes over, continuing after the bytes already read.
#[test]
fn reset_object_completes_from_new_stream() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(loc, 0, 6) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    let mut buf = [0u8; 8];
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert!(matches!(p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t), Ok(3)));

    p.server.reset_send_subgroup(track_alias, 0, 0, ResetCode::Canceled, &mut p.server_t);
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(loc, 0, 6) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abcdef", &mut p.server_t).unwrap();
    p.advance();

    let event = p.client.recv_new_obj_hdr(track_alias, &mut p.client_t);
    assert!(matches!(event, Err(Error::Done)), "duplicate must not be surfaced: {event:?}");
    let read = p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t);
    assert!(matches!(read, Ok(3)), "{read:?}");
    assert_eq!(&buf[..3], b"def");
    assert_eq!(p.client.obj_pld_received(track_alias, loc), Some(6));
}

/// Ending a subgroup mid-object resets that stream, the subgroup can then
/// continue on a new one.
#[test]
fn fin_mid_object_resets_and_subgroup_continues() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    let first = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(first, 0, 5) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, first, b"ab", &mut p.server_t).unwrap();
    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    assert!(p.server.in_progress_send_subgroups(track_alias).is_empty());

    let second = Location { group: 0, object: 1 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(second, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, second, b"xyz", &mut p.server_t).unwrap();
    p.advance();

    let mut buf = [0u8; 16];
    let raw = p.client_t.recv_stream(3, &mut buf);
    assert!(matches!(raw, Err(StreamError::StreamReset(_))), "first stream must be reset: {raw:?}");
}

/// A stream whose objects are all written is not reset by the delivery
/// timeout while it waits for the next object of its subgroup.
#[test]
fn delivery_timeout_spares_idle_stream() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let subscription = p.client
        .subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions { delivery_timeout: Some(10), ..Default::default() }, &mut p.client_t)
        .unwrap();
    let request_id = subscription.request_id();
    p.advance();
    let request = next_subscribe(&mut p.server);
    let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    subscribe_response(&mut p.client, request_id).unwrap().unwrap();

    let loc = Location { group: 0, object: 0 };
    let received_at = Some(p.now);
    p.server.send_obj_hdr(track_alias, OutgoingObject { received_at, ..OutgoingObject::new(loc, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.advance();
    p.now += Duration::from_millis(30);
    p.advance();

    assert_eq!(p.server.in_progress_send_subgroups(track_alias).len(), 1);
    let mut buf = [0u8; 64];
    let raw = p.client_t.recv_stream(3, &mut buf);
    assert!(!matches!(raw, Err(StreamError::StreamReset(_))), "idle stream must not be reset: {raw:?}");
}

/// draft-19 §12.9: an object must not carry two Prior Object ID Gaps.
#[test]
fn send_obj_hdr_rejects_second_prior_object_id_gap() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let with_gap = KeyValuePairs::new().with(KeyValuePair::Varint(moq_wire::property::PRIOR_OBJECT_ID_GAP, 1));

    let loc = Location { group: 0, object: 5 };
    let result = p.server.send_obj_hdr(track_alias, OutgoingObject { prior_object_id_gap: 2, properties: Some(&with_gap), ..OutgoingObject::new(loc, 0, 3) }, &mut p.server_t);
    assert!(matches!(result, Err(Error::InvalidGap)), "{result:?}");
    assert!(p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&with_gap), ..OutgoingObject::new(loc, 0, 3) }, &mut p.server_t).is_ok());
}

/// Once a stream moves on to its next object, the previous object is
/// forgotten, and reading a fully read object is `Done`, not a panic.
#[test]
fn recv_forgets_object_once_stream_moves_on() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    // two objects on one subgroup stream that stays open
    let first = Location { group: 0, object: 0 };
    let second = Location { group: 0, object: 1 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(first, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, first, b"abc", &mut p.server_t).unwrap();
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(second, 0, 5) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, second, b"vwxyz", &mut p.server_t).unwrap();
    p.advance();

    let mut buf = [0u8; 8];
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert!(matches!(p.client.recv_obj_pld(track_alias, first, &mut buf, &mut p.client_t), Ok(3)));
    assert!(matches!(p.client.recv_obj_pld(track_alias, first, &mut buf, &mut p.client_t), Err(Error::Done)));

    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Ok(RecvObjHdr::Header { .. })));
    assert_eq!(p.client.obj_pld_received(track_alias, first), None);
    assert!(matches!(p.client.recv_obj_pld(track_alias, first, &mut buf, &mut p.client_t), Err(Error::Done)));
    assert!(matches!(p.client.recv_obj_pld(track_alias, second, &mut buf, &mut p.client_t), Ok(5)));
    assert_eq!(&buf[..5], b"vwxyz");
}

/// An open, drained subgroup stream must not hide a complete object on a later one.
#[test]
fn recv_new_obj_hdr_not_blocked_by_open_stream() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let kvps = KeyValuePairs::new();

    // group 0: complete, but stream stays open (no FIN), scanned first
    let open = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { properties: Some(&kvps), ..OutgoingObject::new(open, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, open, b"abc", &mut p.server_t).unwrap();

    // group 1: complete, later stream
    let done = Location { group: 1, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { end_of_group: true, properties: Some(&kvps), ..OutgoingObject::new(done, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, done, b"xyz", &mut p.server_t).unwrap();
    p.advance();

    // drain group 0: stream ready for a header but empty
    match p.client.recv_new_obj_hdr(track_alias, &mut p.client_t).unwrap() {
        RecvObjHdr::Header { location, .. } => assert_eq!(location, open),
        other => panic!("expected group 0's header, got {other:?}"),
    }
    let mut buf = [0u8; 8];
    let n = p.client.recv_obj_pld(track_alias, open, &mut buf, &mut p.client_t).unwrap();
    assert_eq!(&buf[..n], b"abc");

    match p.client.recv_new_obj_hdr(track_alias, &mut p.client_t) {
        Ok(RecvObjHdr::Header { location, .. }) => assert_eq!(location, done),
        other => panic!("complete object on a later stream was blocked by an open one: {other:?}"),
    }
}

/// `code` is the whole point of `cancel_send_obj`'s parameter — a caller
/// uses it to tell the peer *why* the object was abandoned
/// (delivery_timeout vs. Forward=0 cancellation, see
/// `RESET_STREAM_CODE_*`). Checks both ends:
/// - the subscriber's own read API (`recv_obj_pld`) sees the abandoned
///   object as done, the same as a clean Fin — this codebase deliberately
///   doesn't distinguish the two at that level (see `in_stream.rs`)
/// - the actual wire-level RESET_STREAM nonetheless carries the exact
///   code passed in, not a hardcoded or dropped one
#[test]
fn cancel_send_obj_resets_stream_with_given_code() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_mock_pipe(config);
    let track_alias = subscribed_track(&mut p);
    let loc = Location { group: 0, object: 0 };

    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, 0, 5), &mut p.server_t).unwrap();
    p.advance();

    // Subscriber sees the header before anything is canceled.
    match p.client.recv_new_obj_hdr(track_alias, &mut p.client_t).unwrap() {
        RecvObjHdr::Header { location, .. } => assert_eq!(location, loc),
        other => panic!("expected a header, got {other:?}"),
    }

    // No send_obj_pld: the object is still mid-payload when canceled.
    p.server.cancel_send_obj(track_alias, loc, ResetCode::Other(0x4321), &mut p.server_t);
    p.advance();

    // Subscriber-observable behavior: the object is done, same signal as
    // a clean Fin — see `InStream::read_obj_pld`'s StreamReset handling.
    let mut buf = [0u8; 16];
    let result = p.client.recv_obj_pld(track_alias, loc, &mut buf, &mut p.client_t);
    assert!(matches!(result, Err(Error::Fin)), "canceled object must read as Fin to the subscriber: {result:?}");

    // The very first server-initiated uni stream this session opens —
    // deterministic under `MockTransport`'s allocation scheme (see
    // `test_utils`): the control channel is the only other stream
    // opened so far, and it's bidi, not uni.
    let subgroup_stream_id = 3;
    let raw = p.client_t.recv_stream(subgroup_stream_id, &mut buf);
    assert!(
        matches!(raw, Err(StreamError::StreamReset(0x4321))),
        "the peer's raw RESET_STREAM must carry the exact code passed to cancel_send_obj: {raw:?}",
    );
}

fn d16_pipe() -> crate::test_utils::MockPipe {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    init_mock_pipe(config)
}

fn send_object(p: &mut crate::test_utils::MockPipe, track_alias: TrackAlias, group: u64, subgroup_id: u64) {
    let loc = Location { group, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject::new(loc, subgroup_id, 1), &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"x", &mut p.server_t).unwrap();
}

/// Reads every available object of `track_alias` on the client.
fn drain_client(p: &mut crate::test_utils::MockPipe, track_alias: TrackAlias) {
    loop {
        match p.client.recv_new_obj_hdr(track_alias, &mut p.client_t) {
            Ok(RecvObjHdr::Header { location, .. }) => {
                let mut buf = [0u8; 16];
                p.client.recv_obj_pld(track_alias, location, &mut buf, &mut p.client_t).unwrap();
            }
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

/// PUBLISH_DONE carries the number of streams opened, so the subscriber
/// keeps reading the ones still in flight.
#[test]
fn publish_done_counts_streams_still_in_flight() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    for group in 0..2 {
        send_object(&mut p, track_alias, group, 0);
        p.server.fin_send_subgroup(track_alias, group, 0, &mut p.server_t);
    }
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    p.advance();

    assert!(!p.client.recv_track_fully_done(track_alias), "its streams are unread");
    drain_client(&mut p, track_alias);
    assert!(p.client.recv_track_fully_done(track_alias));
    assert_eq!(p.client.in_tracks[&track_alias].expected_streams.map(|(count, _)| count), Some(2));
}

/// A stream reset before its header can't be attributed: the subscriber gives up waiting after a timeout.
#[test]
fn publish_done_waits_for_unaccounted_stream_until_timeout() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    send_object(&mut p, track_alias, 0, 0);
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    p.advance();
    drain_client(&mut p, track_alias);
    assert!(!p.client.recv_track_fully_done(track_alias));
    assert_eq!(p.client.timeout_at(), Some(p.now + crate::in_track::PUBLISH_DONE_STREAM_TIMEOUT));

    p.now += crate::in_track::PUBLISH_DONE_STREAM_TIMEOUT;
    p.advance();
    assert!(p.client.recv_track_fully_done(track_alias));
}

/// PUBLISH_DONE resets a subgroup that isn't finished, the subscriber isn't left waiting for it.
#[test]
fn publish_done_resets_unfinished_subgroup() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    send_object(&mut p, track_alias, 0, 0);
    p.advance();
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    assert!(p.server.out_streams.is_empty());
    p.advance();
    drain_client(&mut p, track_alias);
    assert!(p.client.recv_track_fully_done(track_alias));
}

/// After PUBLISH_DONE no object is accepted, the track is forgotten once its streams are delivered.
#[test]
fn ended_out_track_is_removed_once_delivered() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    send_object(&mut p, track_alias, 0, 0);
    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    let late = p.server.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 1, object: 0 }, 0, 1), &mut p.server_t);
    assert!(matches!(late, Err(Error::TrackEnded)), "{late:?}");
    assert!(p.server.out_tracks.contains_key(&track_alias), "kept while its stream is undelivered");

    p.advance();
    p.server.last_stream_sweep = None;
    p.advance();
    assert!(p.server.out_tracks.is_empty());
    assert!(p.server.out_streams.is_empty());
}

/// A received UNSUBSCRIBE resets the undelivered streams and ends the track.
#[test]
fn received_unsubscribe_ends_out_track() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    send_object(&mut p, track_alias, 0, 0);
    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);
    p.advance();
    assert!(p.server.out_tracks.is_empty());
    assert!(p.server.out_streams.is_empty());
    assert!(p.server.out_requests.is_empty());
    let late = p.server.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 1, object: 0 }, 0, 1), &mut p.server_t);
    assert!(matches!(late, Err(Error::TrackEnded)), "{late:?}");
}

/// Unsubscribing a subscription the publisher already ended sends nothing, it only forgets it.
#[test]
fn unsubscribe_after_publish_done_sends_nothing() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    let publication = p.publications.pop().unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    p.advance();
    assert!(p.client.recv_track_fully_done(track_alias));

    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(!p.client.control_channel.has_queued_control_messages());
    assert!(p.client.in_tracks.is_empty());
    assert!(p.client.active_subscriptions.is_empty());
}

/// The PUBLISH_DONE of an accepted PUBLISH reaches its track.
#[test]
fn accepted_publish_receives_publish_done() {
    let mut p = d16_pipe();
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    let (publish_request_id, track_alias) = (publication.request_id(), publication.track_alias());
    p.advance();
    let request = next_publish(&mut p.client);
    let _subscription = p.client.accept_request(request, crate::PublishOk::default(), &mut p.client_t).unwrap();
    p.advance();
    assert!(find_event(&mut p.server, |e| matches!(e, Event::PublishOk { request_id, .. } if request_id == publish_request_id).then_some(())).is_some());
    assert!(p.server.pending_sent_publish.is_empty());

    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    p.advance();
    assert!(p.client.recv_track_fully_done(track_alias));
}

/// A rejected PUBLISH ends our egress track.
#[test]
fn rejected_publish_ends_out_track() {
    let mut p = d16_pipe();
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    let request = next_publish(&mut p.client);
    p.client.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.client_t).unwrap();
    p.advance();
    assert!(!p.server.out_tracks.contains_key(&track_alias));
    assert!(p.server.out_requests.is_empty());
}

#[test]
fn no_mutual_version_closes_session() {
    let config = |v| Config { version_negotiation: VersionNegotiation::InBand { candidates: vec![v] }, ..Config::default() };
    let p = crate::test_utils::connect_mock_pipe(config(moq_wire::MOQ_VERSION_DRAFT_12), config(MOQ_VERSION_DRAFT_11));
    assert!(p.server.closed);
    assert_eq!(p.server_t.closed_with, Some(SessionCloseCode::VersionNegotiationFailed));
}

fn subscribe(p: &mut crate::test_utils::MockPipe) -> Subscription {
    p.client.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut p.client_t).unwrap()
}

/// A responder may stop our side of the request stream along with its REQUEST_ERROR and FIN:
/// the answer counts, not a cancellation (draft-ietf-moq-transport-18 §3.3.2).
#[test]
fn rejection_with_stop_sending_is_a_rejection() {
    let version = moq_wire::MOQ_VERSION_DRAFT_18;
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let subscription = subscribe(&mut p);
    let request_id = subscription.request_id();
    p.advance();
    let request = next_subscribe(&mut p.server);
    p.server.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.server_t).unwrap();
    // both bidi streams the client may have opened for it
    p.client_t.stopped.extend([0, 4]);
    p.advance();
    assert!(matches!(subscribe_response(&mut p.client, request_id), Some(Err(RequestFailure::Rejected(e))) if e.error_code() == RequestErrorCode::DoesNotExist));
}

#[test]
fn dropped_request_is_rejected() {
    let mut p = d16_pipe();
    let subscription = subscribe(&mut p);
    let request_id = subscription.request_id();
    p.advance();
    drop(next_subscribe(&mut p.server));
    p.advance();
    assert!(p.server.unanswered_requests.is_empty());
    p.advance();
    assert!(matches!(subscribe_response(&mut p.client, request_id), Some(Err(RequestFailure::Rejected(e))) if e.error_code() == RequestErrorCode::InternalError));
}

#[test]
fn request_cancelled_by_peer_is_not_answered() {
    let mut p = d16_pipe();
    let subscription = subscribe(&mut p);
    p.advance();
    let request = next_subscribe(&mut p.server);
    p.client.unsubscribe(subscription, &mut p.client_t);
    p.advance();
    assert!(matches!(p.server.next_event(), Some(Event::RequestCancelled(id)) if id == request.request_id()));
    let answer = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t);
    assert!(matches!(answer, Err(Error::RequestCancelled)));
    assert!(p.server.out_tracks.is_empty());
}

#[test]
fn request_cancelled_before_handed_out_is_skipped() {
    let mut p = d16_pipe();
    let subscription = subscribe(&mut p);
    p.advance();
    p.client.unsubscribe(subscription, &mut p.client_t);
    p.advance();
    assert!(p.server.next_event().is_none(), "neither the request nor its cancellation");
    assert!(p.server.unanswered_requests.is_empty());
}

#[test]
#[should_panic(expected = "did not create it")]
fn request_answered_on_another_session_panics() {
    let mut p = d16_pipe();
    let mut q = d16_pipe();
    let _subscription = subscribe(&mut p);
    p.advance();
    let request = next_subscribe(&mut p.server);
    let _ = q.server.accept_request(request, SubscribeOk::default(), &mut q.server_t);
}

#[cfg(feature = "std")]
#[test]
fn shared_drop_queue_is_send() {
    use crate::request::SharedDropQueue;
    fn assert_send<T: Send>() {}
    assert_send::<MoqTransportSession<SharedDropQueue>>();
    assert_send::<IncomingRequest<SharedDropQueue>>();
}

fn d17_pipe() -> crate::test_utils::MockPipe {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(moq_wire::MOQ_VERSION_DRAFT_17), ..Config::default() };
    init_mock_pipe(config)
}

/// Draft 17+: a PUBLISH without a request stream to send it on uses up neither its Request ID nor its Track Alias.
#[test]
fn draft_17_publish_without_a_stream_keeps_its_ids() {
    let mut p = d17_pipe();
    p.server_t.streams_blocked = true;
    assert!(p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).is_err());
    p.server_t.streams_blocked = false;
    let publication = p.server.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    assert_eq!((publication.request_id(), publication.track_alias()), (RequestId(1), TrackAlias(0)));
}

/// The peer's ANNOUNCE_OK / PUBLISH_NAMESPACE_OK / REQUEST_OK marks the namespace accepted,
/// a later PUBLISH_NAMESPACE_DONE needs that.
#[test]
fn accepted_namespace_publish_is_acknowledged() {
    for version in [MOQ_VERSION_DRAFT_10, moq_wire::MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17] {
        let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
        let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();
        let request_id = announcement.request_id();
        p.advance();
        let request = next_publish_namespace(&mut p.server);
        p.server.accept_request(request, (), &mut p.server_t).unwrap();
        p.advance();
        assert!(!p.client.closed && !p.server.closed, "{version}");
        assert!(find_event(&mut p.client, |e| matches!(e, Event::PublishNamespaceOk { request_id: id } if id == request_id).then_some(())).is_some(), "{version}");
    }
}

/// ANNOUNCE_ERROR (drafts 7–13), PUBLISH_NAMESPACE_ERROR (draft 14) or REQUEST_ERROR (draft 15+).
#[test]
fn dropped_namespace_publish_is_rejected() {
    for version in [moq_wire::MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, moq_wire::MOQ_VERSION_DRAFT_11, moq_wire::MOQ_VERSION_DRAFT_14, moq_wire::MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17] {
        let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
        let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();
        let request_id = announcement.request_id();
        p.advance();
        drop(next_publish_namespace(&mut p.server));
        p.advance();
        p.advance();
        assert!(!p.client.closed && !p.server.closed, "{version}");
        let rejected = find_event(&mut p.client, |e| matches!(e, Event::RequestFailed { request_id: id, failure: RequestFailure::Rejected(ref error) } if id == request_id && error.error_code() == RequestErrorCode::InternalError).then_some(()));
        assert!(rejected.is_some(), "{version}");
    }
}

/// Each subscriber's priority applies to its own egress track's streams,
/// also to streams already open (draft-16 §7).
#[test]
fn subscriber_priority_applies_per_egress_track() {
    let mut p = d16_pipe();
    let mut aliases = Vec::new();
    for track in ["n1--t1", "n1--t2"] {
        let subscription = p.client
            .subscribe(&track.parse().unwrap(), SubscribeOptions::default(), &mut p.client_t)
            .unwrap();
        let request_id = subscription.request_id();
        p.advance();
        let request = next_subscribe(&mut p.server);
        let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
        aliases.push(publication.track_alias());
        p.publications.push(publication);
        p.advance();
        subscribe_response(&mut p.client, request_id).unwrap().unwrap();
        p.subscriptions.push(subscription);
    }
    let (urgent, background) = (aliases[0], aliases[1]);
    p.server.set_out_track_priority(urgent, 10, false, &mut p.server_t);
    p.server.set_out_track_priority(background, 200, false, &mut p.server_t);
    send_object(&mut p, urgent, 0, 0);
    send_object(&mut p, background, 0, 0);
    let stream_of = |p: &crate::test_utils::MockPipe, alias| p.server.out_tracks[&alias].open_streams.values().flatten().copied().next().unwrap();
    let (urgent_stream, background_stream) = (stream_of(&p, urgent), stream_of(&p, background));
    assert_eq!(p.server_t.urgencies[&urgent_stream], object_urgency(10));
    assert_eq!(p.server_t.urgencies[&background_stream], object_urgency(200));

    p.server.set_out_track_priority(background, 0, false, &mut p.server_t);
    assert_eq!(p.server_t.urgencies[&background_stream], object_urgency(0), "re-prioritized while open");
    assert_eq!(p.server_t.urgencies[&urgent_stream], object_urgency(10));
}

/// Draft 17 has no UNSUBSCRIBE: the subscriber cancels its request, the publisher ends the subscription.
#[test]
fn draft_17_unsubscribe_cancels_the_request() {
    let mut p = d17_pipe();
    let track_alias = subscribed_track(&mut p);
    let subscription = p.subscriptions.pop().unwrap();
    let request_id = subscription.request_id();
    let stream_id = p.client.control_channel.request_stream(request_id).unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(!p.client.control_channel.has_queued_control_messages(), "no UNSUBSCRIBE");
    assert_eq!(p.client.control_channel.request_stream(request_id), None);
    assert!(p.client.dropped_in_streams.contains(&stream_id), "the mock ignores STOP_SENDING: drained");
    p.advance();
    assert!(!p.server.closed);
    assert!(matches!(p.server.next_event(), Some(Event::Unsubscribed { track_alias: alias, .. }) if alias == track_alias));
    assert!(p.server.out_requests.is_empty());
}


/// Draft 17: PUBLISH_DONE is the request stream's last message.
#[test]
fn draft_17_publish_done_ends_request_stream() {
    let mut p = d17_pipe();
    let track_alias = subscribed_track(&mut p);
    let publication = p.publications.pop().unwrap();
    let server_request_id = publication.request_id();
    let stream_id = p.server.control_channel.request_stream(server_request_id).unwrap();
    p.server.publish_done(publication, PublishDoneStatus::TrackEnded, &mut p.server_t);
    assert_eq!(p.server.control_channel.request_stream(server_request_id), None);
    p.advance();
    assert!(p.server_t.stream_finished(stream_id), "FIN'd");
    assert!(p.client.recv_track_fully_done(track_alias));

    let subscription = p.subscriptions.pop().unwrap();
    let request_id = subscription.request_id();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(!p.client.dropped_in_streams.contains(&stream_id), "already ended by the publisher, no STOP_SENDING");
    assert_eq!(p.client.control_channel.request_stream(request_id), None);
}

/// Draft 17 encodes the padding stream type as a leading-ones varint, not as a QUIC varint.
#[test]
fn draft_17_padding_stream_type_is_leading_ones_varint() {
    let mut p = d17_pipe();
    let stream_id = p.client_t.open_uni_stream().unwrap();
    let mut ty = vec![];
    moq_wire::MOQ_VERSION_DRAFT_17.writer(&mut ty).varint(moq_wire::PADDING_STREAM_TYPE_ID).unwrap();
    assert_ne!(ty[0] >> 6, 0b10, "differs from the 4 byte QUIC varint");
    p.client_t.send_stream(stream_id, &ty, false).unwrap();
    p.client_t.send_stream(stream_id, &[0; 32], false).unwrap();
    p.advance();
    assert!(!p.server.closed);
    assert!(matches!(p.server.in_streams.get_mut(&stream_id).unwrap().handle(&mut p.server_t), Ok(Handled::Padding)));
}

/// A stream that ends inside its header is forgotten.
#[test]
fn fin_inside_stream_header_ends_stream() {
    let mut p = d16_pipe();
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // subgroup header type 0x10, the track alias is missing
    p.server_t.send_stream(stream_id, &[0x10], true).unwrap();
    p.advance();
    assert!(p.client.in_streams.is_empty());
    assert!(!p.client.closed);
}

/// Streams of an alias no track claims are stopped after a timeout, not kept forever.
#[test]
fn stray_stream_is_stopped_after_timeout() {
    let mut p = d16_pipe();
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // subgroup header type 0x10 for unknown alias 9, group 0, subgroup 0, priority
    p.server_t.send_stream(stream_id, &[0x10, 9, 0, 0, 0x80], false).unwrap();
    p.advance();
    assert!(p.client.pending_streams.contains_key(&TrackAlias(9)));
    assert_eq!(p.client.timeout_at(), Some(p.now + STRAY_STREAM_TIMEOUT));

    p.now += STRAY_STREAM_TIMEOUT;
    p.advance();
    assert!(p.client.pending_streams.is_empty());
    assert!(!p.client.in_streams.contains_key(&stream_id));
    assert!(p.client.dropped_in_streams.contains(&stream_id), "the mock ignores STOP_SENDING: drained");
}

/// A dropped alias is forgotten after a timeout.
#[test]
fn dropped_alias_is_forgotten_after_timeout() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    let subscription = p.subscriptions.pop().unwrap();
    p.client.unsubscribe(subscription, &mut p.client_t);
    assert!(p.client.dropped_aliases.contains_key(&track_alias));

    p.now += STRAY_STREAM_TIMEOUT;
    p.advance();
    assert!(p.client.dropped_aliases.is_empty());
}

/// Only the newest groups are remembered for the Prior Object ID Gap
/// check, a gap in a forgotten group is refused instead of unchecked.
#[test]
fn gap_in_forgotten_group_is_refused() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    for group in 0..100 {
        send_object(&mut p, track_alias, group, 0);
        p.server.fin_send_subgroup(track_alias, group, 0, &mut p.server_t);
    }
    let gap = |p: &mut crate::test_utils::MockPipe, group| {
        p.server.send_obj_hdr(track_alias, OutgoingObject { prior_object_id_gap: 2, ..OutgoingObject::new(Location { group, object: 5 }, 1, 1) }, &mut p.server_t)
    };
    assert!(matches!(gap(&mut p, 0), Err(Error::InvalidGap)));
    assert!(gap(&mut p, 99).is_ok());
}

#[test]
fn rejected_namespace_publish_is_reported() {
    let mut p = d16_pipe();
    let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();
    let request_id = announcement.request_id();
    p.advance();
    let request = next_publish_namespace(&mut p.server);
    p.server.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.server_t).unwrap();
    p.advance();
    assert!(matches!(p.client.next_event(), Some(Event::RequestFailed { request_id: id, .. }) if id == request_id));
    assert!(p.client.pending_sent_publish_namespace.is_empty());
}

#[test]
fn dropped_subscription_unsubscribes() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    drop(p.subscriptions.pop());
    p.advance();
    p.advance();
    assert!(p.client.active_subscriptions.is_empty());
    assert!(matches!(p.server.next_event(), Some(Event::Unsubscribed { track_alias: alias, .. }) if alias == track_alias));
}

#[test]
fn dropping_a_handle_wakes_the_drop_waker() {
    struct Counter(core::sync::atomic::AtomicUsize);
    impl alloc::task::Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
    }
    let mut p = d16_pipe();
    subscribed_track(&mut p);
    let counter = Arc::new(Counter(Default::default()));
    p.client.set_drop_waker(counter.clone().into());
    drop(p.subscriptions.pop());
    assert_eq!(counter.0.load(core::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn dropped_publication_sends_publish_done() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    drop(p.publications.pop());
    p.advance();
    p.advance();
    assert!(p.server.out_requests.is_empty());
    assert!(p.client.in_tracks[&track_alias].publish_done_received());
}

/// Longer than one read, within the 64 KiB a control message may have.
#[test]
fn long_control_message_is_received() {
    let mut p = d16_pipe();
    let nt: NamespaceTrackname = format!("{}--t1", "n".repeat(3000)).parse().unwrap();
    let _subscription = p.client.subscribe(&nt, SubscribeOptions::default(), &mut p.client_t).unwrap();
    p.advance();
    assert!(next_subscribe(&mut p.server).message().namespace_trackname == nt);
}

#[test]
fn oversized_object_header_closes_session() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    assert!(track_alias.0 < 64, "single byte varint");
    let stream_id = p.server_t.open_uni_stream().unwrap();
    // subgroup header, then object id 0 with 5000 bytes of properties
    let mut bytes = vec![0x15, track_alias.0 as u8, 0, 0, 0, 0, 0x40 | (5000 >> 8) as u8, (5000 & 0xff) as u8];
    bytes.resize(bytes.len() + 5000, 0);
    p.server_t.send_stream(stream_id, &bytes, false).unwrap();
    p.advance();
    assert!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t).is_err());
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

#[test]
fn duplicate_track_alias_closes_session() {
    let mut p = d16_pipe();
    let track_alias = subscribed_track(&mut p);
    p.server.next_out_track_alias = track_alias;
    let _publication = p.server.publish_track("n1--t2".parse().unwrap(), KeyValuePairs::new(), true, None, &mut p.server_t).unwrap();
    p.advance();
    assert_eq!(p.client_t.closed_with, Some(SessionCloseCode::DuplicateTrackAlias));
}

/// A peer's STOP_SENDING on the control stream closes the session, nothing panics.
#[test]
fn stopped_control_stream_closes_session() {
    let mut p = d16_pipe();
    p.server_t.stopped.insert(0);
    let announced = p.server.publish_namespace("n1".parse().unwrap(), &mut p.server_t);
    assert!(matches!(announced, Err(Error::SessionClosed)));
    assert_eq!(p.server_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// A request answered after the session closed reaches no one.
#[test]
fn request_answered_after_close_fails() {
    let mut p = d16_pipe();
    let _subscription = subscribe(&mut p);
    p.advance();
    let request = next_subscribe(&mut p.server);
    p.server_t.stopped.insert(0);
    assert!(matches!(p.server.publish_namespace("n1".parse().unwrap(), &mut p.server_t), Err(Error::SessionClosed)));
    let answer = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t);
    assert!(matches!(answer, Err(Error::SessionClosed)));
}

/// Draft 17+ replies carry no Request ID: the second request's reply resolves by its stream, not as request 0.
#[test]
fn draft_17_reply_resolves_by_stream() {
    let mut p = d17_pipe();
    let first = subscribe(&mut p);
    let second = subscribe(&mut p);
    p.advance();
    let requests = [next_subscribe(&mut p.server), next_subscribe(&mut p.server)];
    let [_first_request, second_request] = if requests[0].request_id() == first.request_id() { requests } else { let [a, b] = requests; [b, a] };
    p.server.reject_request(second_request, RequestErrorCode::DoesNotExist, &mut p.server_t).unwrap();
    p.advance();
    assert!(matches!(subscribe_response(&mut p.client, second.request_id()), Some(Err(_))));
    assert!(p.client.pending_subscribe.contains_key(&first.request_id()));
}

/// A reply on a stream of no request of ours is dropped, it doesn't end request 0.
#[test]
fn reply_for_no_known_request_is_dropped() {
    let mut p = d17_pipe();
    let subscription = subscribe(&mut p);
    let request_id = subscription.request_id();
    let reply = ControlMessageEnum::RequestError(RequestErrorMessage::new(RequestId(0), RequestErrorCode::DoesNotExist));
    p.client.dispatch_received_message(999, reply, &mut p.client_t);
    assert!(!p.client.closed);
    assert!(p.client.pending_subscribe.contains_key(&request_id));
}

/// PUBLISH_NAMESPACE_DONE names the namespace until draft 15, the Request ID in draft 16,
/// from draft 17 cancelling the request withdraws it.
#[rstest::rstest]
fn withdrawn_namespace_ends_the_peers_namespace(
    #[values(moq_wire::MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, moq_wire::MOQ_VERSION_DRAFT_11, moq_wire::MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17)] version: Version,
) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();
    p.advance();
    let request = next_publish_namespace(&mut p.server);
    p.server.accept_request(request, (), &mut p.server_t).unwrap();
    p.advance();
    assert_eq!(p.server.received_namespaces.len(), 1);
    drop(announcement);
    p.advance();
    p.advance();
    assert!(!p.client.closed && !p.server.closed);
    assert!(p.server.received_namespaces.is_empty());
    let withdrawn = find_event(&mut p.server, |e| match e { Event::NamespaceWithdrawn { namespace, .. } => Some(namespace), _ => None });
    assert_eq!(withdrawn, Some("n1".parse().unwrap()), "{version}");
}

/// An update the publisher accepts reaches it and is answered, one it rejects leaves the subscription.
/// From draft 17 both go on the subscription's stream (draft-ietf-moq-transport-19 §10.9).
#[rstest::rstest]
fn request_update_is_answered_on_its_request(
    #[values(MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17, moq_wire::MOQ_VERSION_DRAFT_19)] version: Version,
    #[values(true, false)] accept: bool,
) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    subscribed_track(&mut p);
    let subscription_id = p.subscriptions[0].request_id();
    let largest = Location { group: 3, object: 0 };
    let update_id = p.client.request_update_forward(&p.subscriptions[0], false, None, &mut p.client_t).unwrap();
    p.advance();
    let updated = find_event(&mut p.server, |e| match e {
        Event::SubscriptionUpdated { request_id, forward, .. } => Some((request_id, forward)),
        _ => None,
    });
    let publication_id = p.publications[0].request_id();
    assert_eq!(updated, Some((publication_id, Some(false))), "{version}");
    let Some(IncomingRequest::RequestUpdate(request)) = next_request(&mut p.server) else { panic!("expected a REQUEST_UPDATE") };
    match accept {
        true => p.server.accept_request(request, Some(largest), &mut p.server_t).unwrap(),
        false => p.server.reject_request(request, RequestErrorCode::NotSupported, &mut p.server_t).unwrap(),
    }
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
    let answer = find_event(&mut p.client, |e| match e {
        Event::RequestUpdateOk { request_id, largest_location } if request_id == update_id => Some(Ok(largest_location)),
        Event::RequestFailed { request_id, .. } if request_id == update_id => Some(Err(())),
        _ => None,
    });
    assert_eq!(answer, Some(if accept { Ok(Some(largest)) } else { Err(()) }), "{version}");
    assert!(p.client.active_subscriptions.contains_key(&subscription_id), "{version}");
}

/// From draft 17 the peer may cancel our requests (draft-ietf-moq-transport-19 §3.3.3): a pending one fails.
#[test]
fn draft_17_pending_subscribe_cancelled_by_the_publisher_fails() {
    let mut p = d17_pipe();
    let subscription = subscribe(&mut p);
    p.advance();
    let request = next_subscribe(&mut p.server);
    p.server.cancel_request_stream(request.request_id(), &mut p.server_t);
    p.advance();
    assert!(!p.client.closed && !p.server.closed);
    assert!(matches!(subscribe_response(&mut p.client, subscription.request_id()), Some(Err(RequestFailure::Cancelled))));
    assert!(p.client.pending_subscribe.is_empty());
}

/// An active subscription the publisher cancels ends as by PUBLISH_DONE.
#[test]
fn draft_17_active_subscription_cancelled_by_the_publisher_ends() {
    let mut p = d17_pipe();
    let track_alias = subscribed_track(&mut p);
    let publication_id = p.publications[0].request_id();
    p.server.cancel_request_stream(publication_id, &mut p.server_t);
    p.advance();
    assert!(!p.client.closed && !p.server.closed);
    assert!(matches!(p.client.recv_new_obj_hdr(track_alias, &mut p.client_t), Err(Error::Fin)));
}

/// An accepted namespace the subscriber cancels is reported to its publisher.
#[test]
fn draft_17_namespace_cancelled_by_the_subscriber_fails() {
    let mut p = d17_pipe();
    let announcement = p.client.publish_namespace("n1".parse().unwrap(), &mut p.client_t).unwrap();
    p.advance();
    let request = next_publish_namespace(&mut p.server);
    let request_id = request.request_id();
    p.server.accept_request(request, (), &mut p.server_t).unwrap();
    p.advance();
    p.server.cancel_request_stream(request_id, &mut p.server_t);
    p.advance();
    assert!(!p.client.closed && !p.server.closed);
    let failed = find_event(&mut p.client, |e| matches!(e, Event::RequestFailed { request_id: id, failure: RequestFailure::Cancelled } if id == announcement.request_id()).then_some(()));
    assert!(failed.is_some());
    assert!(p.client.sent_namespaces.is_empty());
}

/// A STOP_SENDING alone cancels too, also on a request stream with nothing to write, e.g. after the subscriber's FIN.
#[test]
fn draft_17_stop_sending_alone_cancels_the_subscription() {
    let mut p = d17_pipe();
    let track_alias = subscribed_track(&mut p);
    let publication_id = p.publications[0].request_id();
    let stream_id = p.server.control_channel.request_stream(publication_id).unwrap();
    p.server_t.stopped.insert(stream_id);
    p.advance();
    assert!(!p.server.closed);
    assert!(matches!(find_event(&mut p.server, |e| match e { Event::Unsubscribed { track_alias, .. } => Some(track_alias), _ => None }), Some(alias) if alias == track_alias));
    assert_eq!(p.server.control_channel.request_stream(publication_id), None);
}

/// Either peer sends SETUP as soon as it can, some clients wait for the server's (draft-ietf-moq-transport-18 §3.3).
#[test]
fn draft_17_server_sends_setup_before_the_clients() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(moq_wire::MOQ_VERSION_DRAFT_17), ..Config::default() };
    let mut p = crate::test_utils::unconnected_mock_pipe(config.clone(), config);
    p.server.handle(&mut p.server_t, &[], p.now);
    p.deliver();
    // the server's control stream
    p.client.handle(&mut p.client_t, &[3], p.now);
    assert!(p.client.initialized());
}

/// A request stream read before the control stream waits for SETUP (draft-ietf-moq-transport-19 §3.3).
#[test]
fn draft_17_request_before_setup_waits_for_it() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(moq_wire::MOQ_VERSION_DRAFT_17), ..Config::default() };
    let mut p = crate::test_utils::unconnected_mock_pipe(config.clone(), config);
    let subscription = subscribe(&mut p);
    let request_stream = p.client.control_channel.request_stream(subscription.request_id()).unwrap();
    p.deliver();
    // the client's first bidi stream, then its control stream
    p.server.handle(&mut p.server_t, &[request_stream, 2], p.now);
    assert!(!p.server.closed);
    assert_eq!(next_subscribe(&mut p.server).request_id(), subscription.request_id());
}

/// A data stream read before the uni control stream is told apart by its first byte, which it keeps (draft-ietf-moq-transport-19 §3.3).
#[test]
fn draft_17_data_stream_before_the_control_stream_is_told_apart() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(moq_wire::MOQ_VERSION_DRAFT_17), ..Config::default() };
    let mut p = crate::test_utils::unconnected_mock_pipe(config.clone(), config);
    let padding_stream = p.client_t.open_uni_stream().unwrap();
    let mut ty = vec![];
    moq_wire::MOQ_VERSION_DRAFT_17.writer(&mut ty).varint(moq_wire::PADDING_STREAM_TYPE_ID).unwrap();
    p.client_t.send_stream(padding_stream, &ty, false).unwrap();
    p.deliver();
    // the padding stream, then the client's control stream
    p.server.handle(&mut p.server_t, &[padding_stream, 2], p.now);
    assert!(!p.server.closed);
    assert!(p.server.control_channel.is_control_stream(2));
    assert!(matches!(p.server.in_streams.get_mut(&padding_stream).unwrap().handle(&mut p.server_t), Ok(Handled::Padding)));
}

/// A peer's request stream must begin with a request (draft-ietf-moq-transport-19 §3.3).
#[test]
fn draft_17_request_stream_beginning_with_a_reply_closes_the_session() {
    let mut p = d17_pipe();
    let stream_id = p.client_t.open_bidi_stream().unwrap();
    let reply = ControlMessageEnum::RequestError(RequestErrorMessage::new(RequestId(0), RequestErrorCode::DoesNotExist));
    let mut bytes = vec![];
    moq_wire::ToBytes::to_bytes(&reply, &mut moq_wire::MOQ_VERSION_DRAFT_17.writer(&mut bytes), moq_wire::MOQ_VERSION_DRAFT_17).unwrap();
    p.client_t.send_stream(stream_id, &bytes, false).unwrap();
    p.advance();
    assert_eq!(p.server_t.closed_with, Some(SessionCloseCode::ProtocolViolation));
}

/// SUBSCRIBE_OK names the track alias: the subscriber's from SUBSCRIBE until draft 11, which don't repeat it, the publisher's after.
#[rstest::rstest]
fn subscribe_ok_names_the_track_alias(#[values(moq_wire::MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, moq_wire::MOQ_VERSION_DRAFT_11, moq_wire::MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_16)] version: Version) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let subscription = subscribe(&mut p);
    p.advance();
    let request = next_subscribe(&mut p.server);
    let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
    p.advance();
    let (track_alias, message) = subscribe_response(&mut p.client, subscription.request_id()).unwrap().unwrap();
    assert_eq!((track_alias, message.track_alias()), (publication.track_alias(), publication.track_alias()), "{version}");
}

/// Before draft 15 SUBSCRIBE_UPDATE repeats the whole subscription and has no reply: the publisher applies it,
/// the subscriber takes it as accepted. A relative filter is absolute once SUBSCRIBE_OK named the Largest Object.
#[rstest::rstest]
fn subscribe_update_until_draft_14(#[values(moq_wire::MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, moq_wire::MOQ_VERSION_DRAFT_11, moq_wire::MOQ_VERSION_DRAFT_14)] version: Version) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    subscribed_track(&mut p);
    let forward = version.draft() < 11;
    let update_id = p.client.request_update_forward(&p.subscriptions[0], forward, None, &mut p.client_t).unwrap();
    assert!(find_event(&mut p.client, |e| matches!(e, Event::RequestUpdateOk { request_id, largest_location: None } if request_id == update_id).then_some(())).is_some(), "{version}");
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
    let publication_id = p.publications[0].request_id();
    let mut requests = 0;
    let updated = find_event(&mut p.server, |e| match e {
        Event::SubscriptionUpdated { request_id, forward, filter } => Some((request_id, forward, filter)),
        Event::Request(_) => { requests += 1; None }
        _ => None,
    });
    let start = LocationFilter::AbsoluteStart { start: Location { group: 0, object: 0 } };
    assert_eq!(updated, Some((publication_id, Some(forward), Some(start))), "{version}");
    assert_eq!(requests, 0, "{version}: no reply to give");
    // the next request still has the expected Request ID
    let second = subscribe(&mut p);
    p.advance();
    assert!(!p.server.closed, "{version}");
    assert_eq!(next_subscribe(&mut p.server).request_id(), second.request_id());
}

/// What a draft can't express is refused without sending anything: before draft 11 forwarding can't stop.
#[test]
fn subscribe_update_refuses_what_its_draft_lacks() {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(MOQ_VERSION_DRAFT_10), ..Config::default() });
    subscribed_track(&mut p);
    let refused = p.client.request_update_forward(&p.subscriptions[0], false, None, &mut p.client_t);
    assert!(matches!(refused, Err(Error::Wire(moq_wire::Error::Unsupported(_)))));
    assert!(!p.client.closed);
}

/// From draft 18 a FIN'd subgroup not acked within SUBGROUP_DELIVERY_TIMEOUT is reset, the object timeout
/// no longer covers it (draft-ietf-moq-transport-19 §8). Before, the delivery timeout keeps covering it.
#[rstest::rstest]
#[case::subgroup_timeout(moq_wire::MOQ_VERSION_DRAFT_18, Some(1000), Some(10), true)]
#[case::object_timeout_only(moq_wire::MOQ_VERSION_DRAFT_18, Some(10), None, false)]
#[case::draft_16_delivery_timeout(MOQ_VERSION_DRAFT_16, Some(10), None, true)]
fn fin_unacked_subgroup_expires(#[case] version: Version, #[case] object: Option<u64>, #[case] subgroup: Option<u64>, #[case] reset: bool) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let options = SubscribeOptions { delivery_timeout: object, subgroup_delivery_timeout: subgroup, ..Default::default() };
    let subscription = p.client.subscribe(&"n1--t1".parse().unwrap(), options, &mut p.client_t).unwrap();
    p.advance();
    let request = next_subscribe(&mut p.server);
    let publication = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap();
    let track_alias = publication.track_alias();
    p.advance();
    subscribe_response(&mut p.client, subscription.request_id()).unwrap().unwrap();

    let loc = Location { group: 0, object: 0 };
    p.server.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(p.now), ..OutgoingObject::new(loc, 0, 3) }, &mut p.server_t).unwrap();
    p.server.send_obj_pld(track_alias, loc, b"abc", &mut p.server_t).unwrap();
    p.server.fin_send_subgroup(track_alias, 0, 0, &mut p.server_t);
    // not delivered, so not acked
    p.now += Duration::from_millis(30);
    p.server.handle(&mut p.server_t, &[], p.now);
    assert_eq!(p.server.in_progress_send_subgroups(track_alias).is_empty() && p.server.out_streams.is_empty(), reset, "{version}");
}

/// Draft 19's Range Filters of a SUBSCRIBE: objects not passing aren't sent, more ranges than MAX_FILTER_RANGES are refused.
#[rstest::rstest]
#[case::applied(16, true)]
#[case::beyond_max_filter_ranges(1, false)]
fn draft_19_range_filters(#[case] max_filter_ranges: u64, #[case] accepted: bool) {
    use moq_wire::range_filter::{RangeFilter, RangeFilterKind, RangeFilters};
    let version = moq_wire::MOQ_VERSION_DRAFT_19;
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), max_filter_ranges, ..Config::default() });
    // objects 0 to 1 and from 5
    let filter = RangeFilter { set_id: 0, kind: RangeFilterKind::ObjectId, ranges: vec![(0, Some(1)), (5, None)] };
    let subscribe = SubscribeMessage { range_filters: RangeFilters::new(&[filter], version).unwrap(), ..SubscribeMessage::new(RequestId(0), "n1--t1".parse().unwrap()) };
    let mut bytes = vec![];
    moq_wire::ToBytes::to_bytes(&ControlMessageEnum::Subscribe(subscribe), &mut version.writer(&mut bytes), version).unwrap();
    let stream_id = p.client_t.open_bidi_stream().unwrap();
    p.client_t.send_stream(stream_id, &bytes, false).unwrap();
    p.advance();
    assert!(!p.server.closed);
    let Some(request) = next_request(&mut p.server) else {
        assert!(!accepted, "the SUBSCRIBE is refused with INVALID_FILTER");
        return;
    };
    assert!(accepted);
    let IncomingRequest::Subscribe(request) = request else { panic!("expected a SUBSCRIBE") };
    let track_alias = p.server.accept_request(request, SubscribeOk::default(), &mut p.server_t).unwrap().track_alias();
    let sent: Vec<u64> = (0..7).filter(|&object| {
        p.server.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object }, 0, 0), &mut p.server_t).is_ok()
    }).collect();
    assert_eq!(sent, [0, 1, 5, 6]);
}

/// A request stream of an unknown type is reset, the session stays (draft-ietf-moq-transport-18 §3.3);
/// a SUBSCRIBE_NAMESPACE is a request answered by REQUEST_OK, then NAMESPACE.
#[test]
fn subscribe_namespace_is_a_request_unknown_requests_are_reset() {
    let version = moq_wire::MOQ_VERSION_DRAFT_18;
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    // unassigned (0x52): type, length, body
    let mut raw = vec![];
    let mut w = version.writer(&mut raw);
    w.varint(0x52).unwrap();
    w.u8(0);
    w.u8(2);
    w.u8(0);
    w.u8(0);
    let unknown = p.client_t.open_bidi_stream().unwrap();
    p.client_t.send_stream(unknown, &raw, false).unwrap();
    p.advance();
    p.advance();
    assert!(!p.server.closed, "{:?}", p.server_t.closed_with);
    let mut buf = [0u8; 8];
    let read = p.client_t.recv_stream(unknown, &mut buf);
    assert!(matches!(read, Err(StreamError::StreamReset(_))), "{read:?}, closed with {:?}", p.server_t.closed_with);

    let subscribe_namespace = moq_wire::control_message::SubscribeNamespaceMessage { request_id: RequestId(0), prefix: moq_wire::NamespaceTuple::new(vec![b"n1".to_vec()]).unwrap() };
    let mut bytes = vec![];
    moq_wire::ToBytes::to_bytes(&ControlMessageEnum::SubscribeNamespace(subscribe_namespace), &mut version.writer(&mut bytes), version).unwrap();
    let stream_id = p.client_t.open_bidi_stream().unwrap();
    p.client_t.send_stream(stream_id, &bytes, false).unwrap();
    p.advance();
    let Some(IncomingRequest::SubscribeNamespace(request)) = next_request(&mut p.server) else { panic!("expected a SUBSCRIBE_NAMESPACE") };
    let subscription = p.server.accept_request(request, (), &mut p.server_t).unwrap();
    p.server.send_namespace(&subscription, moq_wire::NamespaceTuple::new(vec![b"a".to_vec()]).unwrap(), &mut p.server_t).unwrap();
    // read here, not by the client session
    p.deliver();
    assert!(!p.server.closed);
    // REQUEST_OK (0x7) then NAMESPACE (0x8) on the request's stream
    let mut buf = [0u8; 32];
    let len = p.client_t.recv_stream(stream_id, &mut buf).unwrap();
    assert_eq!(buf[0], 0x7);
    let request_ok_len = 3 + u16::from_be_bytes([buf[1], buf[2]]) as usize;
    assert_eq!(&buf[request_ok_len..len], [0x8, 0, 3, 1, 1, b'a']);
}

/// SUBSCRIBE_TRACKS is a request answered by REQUEST_OK, its PUBLISHes follow on their own streams
/// (draft-ietf-moq-transport-18 §10.19).
#[test]
fn subscribe_tracks_is_answered_by_request_ok() {
    let version = moq_wire::MOQ_VERSION_DRAFT_18;
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let subscribe_tracks = moq_wire::control_message::SubscribeTracksMessage { request_id: RequestId(0), prefix: moq_wire::NamespaceTuple::new(vec![b"n1".to_vec()]).unwrap(), forward: false, group_order: None };
    let mut bytes = vec![];
    moq_wire::ToBytes::to_bytes(&ControlMessageEnum::SubscribeTracks(subscribe_tracks), &mut version.writer(&mut bytes), version).unwrap();
    let stream_id = p.client_t.open_bidi_stream().unwrap();
    p.client_t.send_stream(stream_id, &bytes, false).unwrap();
    p.advance();
    let Some(IncomingRequest::SubscribeTracks(request)) = next_request(&mut p.server) else { panic!("expected a SUBSCRIBE_TRACKS") };
    assert!(!request.message().forward);
    let subscription = p.server.accept_request(request, (), &mut p.server_t).unwrap();
    p.server.send_publish_blocked(&subscription, moq_wire::NamespaceTuple::new(vec![b"a".to_vec()]).unwrap(), b"t".to_vec(), &mut p.server_t).unwrap();
    p.deliver();
    let mut buf = [0u8; 32];
    let len = p.client_t.recv_stream(stream_id, &mut buf).unwrap();
    assert_eq!(buf[0], 0x7, "REQUEST_OK");
    // then PUBLISH_BLOCKED (§10.20): suffix, track name
    let request_ok_len = 3 + u16::from_be_bytes([buf[1], buf[2]]) as usize;
    assert_eq!(&buf[request_ok_len..len], [0xF, 0, 5, 1, 1, b'a', 1, b't']);
    p.server.end_tracks_subscription(subscription, &mut p.server_t);
    p.deliver();
    while p.client_t.recv_stream(stream_id, &mut buf).is_ok() {}
    assert!(matches!(p.client_t.recv_stream(stream_id, &mut buf), Err(StreamError::Fin)));
}

/// TRACK_STATUS is answered with what a SUBSCRIBE_OK would name: TRACK_STATUS_OK in drafts 13 and 14,
/// a REQUEST_OK from draft 15, or the matching error.
#[rstest::rstest]
fn track_status_is_answered(
    #[values(moq_wire::MOQ_VERSION_DRAFT_13, moq_wire::MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17, moq_wire::MOQ_VERSION_DRAFT_19)] version: Version,
    #[values(true, false)] accept: bool,
) {
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let request_id = p.client.track_status(&"n1--t1".parse().unwrap(), &mut p.client_t).unwrap();
    p.advance();
    let Some(IncomingRequest::TrackStatus(request)) = next_request(&mut p.server) else { panic!("expected a TRACK_STATUS") };
    assert_eq!(request.message().namespace_trackname, "n1--t1".parse().unwrap());
    let largest = Location { group: 4, object: 2 };
    match accept {
        true => p.server.accept_request(request, SubscribeOk { largest_location: Some(largest), max_cache_duration: None }, &mut p.server_t).unwrap(),
        false => p.server.reject_request(request, RequestErrorCode::DoesNotExist, &mut p.server_t).unwrap(),
    }
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
    let answer = find_event(&mut p.client, |e| match e {
        Event::TrackStatus { request_id: id, largest_location, .. } if id == request_id => Some(Ok(largest_location)),
        Event::RequestFailed { request_id: id, failure: RequestFailure::Rejected(error) } if id == request_id => Some(Err(error.error_code())),
        _ => None,
    });
    assert_eq!(answer, Some(if accept { Ok(Some(largest)) } else { Err(RequestErrorCode::DoesNotExist) }), "{version}");
}

fn fetch_object(group: u64, object: u64, payload: &[u8]) -> moq_wire::fetch::FetchEntry {
    moq_wire::fetch::FetchEntry::Object(moq_wire::fetch::FetchObject {
        location: Location { group, object },
        subgroup_id: Some(0),
        publisher_priority: 0,
        properties: KeyValuePairs::new(),
        payload_len: payload.len(),
    })
}

/// A FETCH is answered with FETCH_OK and its objects on a fetch stream, read to its end (draft-ietf-moq-transport-19 §10.12).
#[rstest::rstest]
fn fetch_delivers_its_objects(#[values(moq_wire::MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_17, moq_wire::MOQ_VERSION_DRAFT_19)] version: Version) {
    use moq_wire::control_message::{FetchKind, FetchRange};
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let kind = FetchKind::Standalone { namespace_trackname: "n1--t1".parse().unwrap(), range: FetchRange { start: Location { group: 0, object: 0 }, end: Location { group: 2, object: 0 } } };
    let fetch = p.client.fetch(kind, FetchOptions::default(), &mut p.client_t).unwrap();
    p.advance();
    let Some(IncomingRequest::Fetch(request)) = next_request(&mut p.server) else { panic!("expected a FETCH") };
    let end_location = Location { group: 1, object: 1 };
    let response = p.server.accept_request(request, crate::FetchOk { end_of_track: false, end_location }, &mut p.server_t).unwrap();
    let objects: [(u64, u64, &[u8]); 3] = [(0, 0, b"ab"), (0, 1, b"c"), (1, 0, b"def")];
    for (group, object, payload) in objects {
        p.server.send_fetch_entry(&response, &fetch_object(group, object, payload), &mut p.server_t).unwrap();
        assert_eq!(p.server.send_fetch_pld(&response, payload, &mut p.server_t).unwrap(), payload.len());
    }
    p.server.finish_fetch(response, &mut p.server_t).unwrap();
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
    let ok = find_event(&mut p.client, |e| match e {
        Event::FetchOk { request_id, end_location, .. } if request_id == fetch.request_id() => Some(end_location),
        _ => None,
    });
    assert_eq!(ok, Some(end_location), "{version}");
    for (group, object, payload) in objects {
        assert_eq!(p.client.recv_fetch_entry(&fetch, &mut p.client_t).unwrap(), fetch_object(group, object, payload), "{version}");
        let mut buf = [0u8; 8];
        assert_eq!(p.client.recv_fetch_pld(&fetch, &mut buf, &mut p.client_t).unwrap(), payload.len());
        assert_eq!(&buf[..payload.len()], payload);
    }
    assert!(matches!(p.client.recv_fetch_entry(&fetch, &mut p.client_t), Err(Error::Fin)), "{version}");
    p.client.cancel_fetch(fetch, &mut p.client_t);
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
}

/// Cancelling a fetch stops its answer: FETCH_CANCEL until draft 16, cancelling the request after.
#[rstest::rstest]
fn cancelled_fetch_ends_its_answer(#[values(MOQ_VERSION_DRAFT_16, moq_wire::MOQ_VERSION_DRAFT_19)] version: Version) {
    use moq_wire::control_message::{FetchKind, FetchRange};
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
    let kind = FetchKind::Standalone { namespace_trackname: "n1--t1".parse().unwrap(), range: FetchRange { start: Location { group: 0, object: 0 }, end: Location { group: 2, object: 0 } } };
    let fetch = p.client.fetch(kind, FetchOptions::default(), &mut p.client_t).unwrap();
    p.advance();
    let Some(IncomingRequest::Fetch(request)) = next_request(&mut p.server) else { panic!("expected a FETCH") };
    let request_id = request.request_id();
    let response = p.server.accept_request(request, crate::FetchOk { end_of_track: false, end_location: Location { group: 1, object: 0 } }, &mut p.server_t).unwrap();
    p.advance();
    p.client.cancel_fetch(fetch, &mut p.client_t);
    p.advance();
    assert!(!p.client.closed && !p.server.closed, "{version}");
    assert!(find_event(&mut p.server, |e| matches!(e, Event::FetchCancelled { request_id: id } if id == request_id).then_some(())).is_some(), "{version}");
    assert!(matches!(p.server.send_fetch_entry(&response, &fetch_object(0, 0, b"x"), &mut p.server_t), Err(Error::TrackEnded)));
}

/// Before draft 15 a FETCH is refused, its fetch stream isn't implemented.
#[test]
fn fetch_before_draft_15_is_refused() {
    use moq_wire::control_message::{FetchKind, FetchMessage, FetchRange};
    let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(moq_wire::MOQ_VERSION_DRAFT_14), ..Config::default() });
    let kind = FetchKind::Standalone { namespace_trackname: "n1--t1".parse().unwrap(), range: FetchRange { start: Location { group: 0, object: 0 }, end: Location { group: 2, object: 0 } } };
    assert!(matches!(p.client.fetch(kind.clone(), FetchOptions::default(), &mut p.client_t), Err(Error::Unimplemented)));
    p.client.send_control_message(&mut p.client_t, &ControlMessageEnum::Fetch(FetchMessage::new(RequestId(0), kind)));
    p.advance();
    assert!(!p.server.closed && next_request(&mut p.server).is_none());
}

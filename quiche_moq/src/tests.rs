use crate::{OutgoingObject, Timestamp};
use crate::test_utils::{init_moq_pipe, MoqPipe};
use crate::{Config, Event, IncomingRequest, RecvObjHdr, SubscribeOk, VersionNegotiation};
use moq_core::Error;
use moq_core::test_utils::{find_event, next_publish, next_request, next_subscribe, subscribe_response};
use moq_wire::object::ObjectHeader;
use moq_wire::TrackAlias;
use crate::SubscribeOptions;
use moq_wire::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, Version};
use moq_wire::location::Location;
use moq_wire::KeyValuePairs;
use std::time::Duration;

macro_rules! test_webtransport_moq_versions {
    ($($name:ident: $version:expr,)*) => {
    $(
        #[test]
        fn $name() {
            test_webtransport_moq($version);
        }
    )*
    }
}

test_webtransport_moq_versions! {
    test_webtransport_moq_draft07: MOQ_VERSION_DRAFT_07,
    test_webtransport_moq_draft08: MOQ_VERSION_DRAFT_08,
    test_webtransport_moq_draft09: MOQ_VERSION_DRAFT_09,
    test_webtransport_moq_draft10: MOQ_VERSION_DRAFT_10,
    test_webtransport_moq_draft11: MOQ_VERSION_DRAFT_11,
    test_webtransport_moq_draft12: MOQ_VERSION_DRAFT_12,
    test_webtransport_moq_draft13: MOQ_VERSION_DRAFT_13,
    test_webtransport_moq_draft14: MOQ_VERSION_DRAFT_14,
    test_webtransport_moq_draft15: MOQ_VERSION_DRAFT_15,
    test_webtransport_moq_draft16: MOQ_VERSION_DRAFT_16,
}

#[test]
fn delivery_timeout() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);

    // Subscriber requests with a 5000ms delivery_timeout.
    let mut client = p.client();
    let subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions { delivery_timeout: Some(5000), ..Default::default() }, &mut client.t).unwrap();
    p.advance().unwrap();

    // Publisher sees the delivery_timeout in the SUBSCRIBE.
    let publication = {
        let mut s = p.server();
        let request = next_subscribe(s.session);
        assert_eq!(request.message().object_delivery_timeout, Some(5000));
        s.session.accept_request(request, SubscribeOk::default(), &mut s.t).unwrap()
    };
    let track_alias = publication.track_alias();
    p.advance().unwrap();

    subscribe_response(p.client().session, subscription.request_id()).unwrap().unwrap();

    // A fresh object (received now) is within the timeout and sends successfully.
    let now = Timestamp::now();
    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(now), ..OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5) }, &mut server.t).unwrap();

    // An old object exceeds the timeout and returns ObjectExpired.
    let old = Timestamp::now() - Duration::from_secs(10);
    let result = server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(old), ..OutgoingObject::new(Location { group: 1, object: 0 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // Subscriber sends REQUEST_UPDATE increasing the delivery_timeout to 60 seconds.
    let mut client = p.client();
    client.session.request_update_delivery_timeout(&subscription, 60_000, &mut client.t).unwrap();
    p.advance().unwrap();

    // After updating, the 10-second-old object is within the new 60s timeout.
    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(old), ..OutgoingObject::new(Location { group: 2, object: 0 }, 0, 5) }, &mut server.t).unwrap();
}

#[test]
fn max_cache_duration() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);

    let mut client = p.client();
    let subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut client.t).unwrap();
    p.advance().unwrap();

    let request = next_subscribe(p.server().session);
    // Accept with a 5000ms max_cache_duration.
    let ok = SubscribeOk { largest_location: Some(Location { group: 1, object: 0 }), max_cache_duration: Some(5000) };
    let mut server = p.server();
    let publication = server.session.accept_request(request, ok, &mut server.t).unwrap();
    let track_alias = publication.track_alias();
    p.advance().unwrap();

    // Subscriber receives max_cache_duration in SUBSCRIBE_OK.
    let (_, ok_msg) = subscribe_response(p.client().session, subscription.request_id()).unwrap().unwrap();
    assert_eq!(ok_msg.max_cache_duration(), Some(5000));
    assert_eq!(ok_msg.largest_location(), Some(Location { group: 1, object: 0 }));

    // A fresh object is within max_cache_duration and sends successfully.
    let now = Timestamp::now();
    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(now), ..OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5) }, &mut server.t).unwrap();

    // An old object exceeds max_cache_duration and returns ObjectExpired.
    let old = Timestamp::now() - Duration::from_secs(10);
    let result = server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(old), ..OutgoingObject::new(Location { group: 1, object: 0 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);
}

/// Models `moq_cli::node`'s relay delivery_timeout aggregation: a
/// relay subscribes upstream with the max `delivery_timeout` across all of
/// its downstream subscribers, and raises that subscription's
/// delivery_timeout via REQUEST_UPDATE if a later subscriber needs more
/// leniency than the relay originally requested. This exercises the exact
/// `subscribe` + `request_update_delivery_timeout` sequence the relay's
/// aggregation logic performs against the upstream publisher.
#[test]
fn relay_raises_upstream_delivery_timeout_via_request_update() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);

    // Relay subscribes upstream with its first (only) downstream
    // subscriber's delivery_timeout: 1000ms.
    let mut client = p.client();
    let subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions { delivery_timeout: Some(1_000), ..Default::default() }, &mut client.t).unwrap();
    p.advance().unwrap();
    let publication = {
        let mut s = p.server();
        let request = next_subscribe(s.session);
        s.session.accept_request(request, SubscribeOk::default(), &mut s.t).unwrap()
    };
    let track_alias = publication.track_alias();
    p.advance().unwrap();
    subscribe_response(p.client().session, subscription.request_id()).unwrap().unwrap();

    // A 4-second-old object exceeds the 1000ms timeout.
    let old = Timestamp::now() - Duration::from_secs(4);
    let mut server = p.server();
    let result = server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(old), ..OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // A second downstream subscriber joins wanting a 5000ms timeout. The
    // relay's aggregate is now max(1000, 5000) = 5000, raised upstream via
    // REQUEST_UPDATE rather than a new SUBSCRIBE.
    let mut client = p.client();
    client.session.request_update_delivery_timeout(&subscription, 5_000, &mut client.t).unwrap();
    p.advance().unwrap();

    // The same 4-second-old object is now within the raised 5000ms timeout.
    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject { received_at: Some(old), ..OutgoingObject::new(Location { group: 1, object: 0 }, 0, 5) }, &mut server.t).unwrap();
}

/// Models `moq_cli::node`'s relay max_cache_duration propagation:
/// the relay reads `max_cache_duration` from its own SUBSCRIBE_OK from the
/// upstream publisher and passes that same value to `accept_request`
/// for every downstream subscriber, so their `OutTrack` enforces the same
/// expiry the upstream publisher promised. Uses two independent pipes (the
/// relay's upstream-facing session and its downstream-facing session are
/// genuinely separate `MoqTransportSession`s over separate connections, just
/// like in moq_cli) and threads the value between them the same way.
#[test]
fn relay_propagates_max_cache_duration_from_upstream_to_downstream() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };

    // Pipe A: origin publisher (server) <-> relay's upstream-facing session (client).
    let mut upstream_pipe = init_moq_pipe(config.clone());
    // Pipe B: relay's downstream-facing session (server) <-> subscriber (client).
    let mut downstream_pipe = init_moq_pipe(config);

    let mut client = downstream_pipe.client();
    let down_subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut client.t).unwrap();
    downstream_pipe.advance().unwrap();

    let mut client = upstream_pipe.client();
    let up_subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut client.t).unwrap();
    upstream_pipe.advance().unwrap();

    // Origin accepts with an 8-second max_cache_duration.
    let request = next_subscribe(upstream_pipe.server().session);
    let mut server = upstream_pipe.server();
    let _publication = server.session.accept_request(request, SubscribeOk { largest_location: None, max_cache_duration: Some(8_000) }, &mut server.t).unwrap();
    upstream_pipe.advance().unwrap();
    let (_, ok_msg) = subscribe_response(upstream_pipe.client().session, up_subscription.request_id()).unwrap().unwrap();
    let max_cache_duration = ok_msg.max_cache_duration();
    assert_eq!(max_cache_duration, Some(8_000));

    // Relay propagates that same value to its own downstream subscriber's SUBSCRIBE_OK.
    let relay_publication = {
        let mut s = downstream_pipe.server();
        let request = next_subscribe(s.session);
        s.session.accept_request(request, SubscribeOk { largest_location: None, max_cache_duration }, &mut s.t).unwrap()
    };
    let relay_alias = relay_publication.track_alias();
    downstream_pipe.advance().unwrap();
    subscribe_response(downstream_pipe.client().session, down_subscription.request_id()).unwrap().unwrap();

    // A 9-second-old object exceeds the propagated 8s max_cache_duration...
    let too_old = Timestamp::now() - Duration::from_secs(9);
    let mut server = downstream_pipe.server();
    let result = server.session.send_obj_hdr(relay_alias, OutgoingObject { received_at: Some(too_old), ..OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // ...while a 7-second-old object is still within it.
    let within = Timestamp::now() - Duration::from_secs(7);
    server.session.send_obj_hdr(relay_alias, OutgoingObject { received_at: Some(within), ..OutgoingObject::new(Location { group: 1, object: 0 }, 0, 5) }, &mut server.t).unwrap();
}

/// PUBLISH (0x1D) flow: publisher sends PUBLISH, subscriber accepts with PUBLISH_OK,
/// publisher streams an object, subscriber reads it.
#[test]
fn publish_track() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);

    // Publisher (server side) sends PUBLISH and immediately streams an object.
    let mut server = p.server();
    let publication = server.session.publish_track("n1--t1".parse().unwrap(), KeyValuePairs::new(), true, None, &mut server.t).unwrap();
    let (pub_request_id, track_alias) = (publication.request_id(), publication.track_alias());
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5), &mut server.t).unwrap();
    let n = server.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, b"hello", &mut server.t).unwrap();
    assert_eq!(n, 5);
    p.advance().unwrap();

    // Subscriber (client side) receives the PUBLISH and accepts it.
    p.client().poll_all();
    let request = next_publish(p.client().session);
    let mut client = p.client();
    let _subscription = client.session.accept_request(request, crate::PublishOk::default(), &mut client.t).unwrap();
    p.advance().unwrap();

    // Publisher receives PUBLISH_OK.
    p.server().poll_all();
    assert!(find_event(p.server().session, |e| matches!(e, Event::PublishOk { request_id, .. } if request_id == pub_request_id).then_some(())).is_some());

    // Subscriber reads the object that arrived before PUBLISH_OK.
    let mut client = p.client();
    let track_alias = *client.session.readable().first().unwrap();
    let (location, _subgroup_id, _hdr) = expect_hdr(client.session.recv_new_obj_hdr(track_alias, &mut client.t));
    let mut buf = [0u8; 10];
    let n = client.session.recv_obj_pld(track_alias, location, &mut buf, &mut client.t).unwrap();
    assert_eq!(&buf[..n], b"hello");
}

/// Unwraps a `recv_new_obj_hdr` result expected to be a new header, not a
/// `SubgroupFinished` event.
fn expect_hdr(r: Result<RecvObjHdr, Error>) -> (Location, u64, ObjectHeader) {
    match r.unwrap() {
        RecvObjHdr::Header { location, subgroup_id, header, .. } => (location, subgroup_id, header),
        other => panic!("expected a new object header, got {other:?}"),
    }
}

/// Sets up a subscribed track and returns its `track_alias` — shared setup
/// for the `send_obj_hdr`/`send_obj_pld` tests below.
fn subscribed_track(p: &mut MoqPipe) -> TrackAlias {
    let mut client = p.client();
    let subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut client.t).unwrap();
    p.advance().unwrap();
    let publication = {
        let mut s = p.server();
        let request = next_subscribe(s.session);
        s.session.accept_request(request, SubscribeOk::default(), &mut s.t).unwrap()
    };
    let track_alias = publication.track_alias();
    p.advance().unwrap();
    subscribe_response(p.client().session, subscription.request_id()).unwrap().unwrap();
    p.subscriptions.push(subscription);
    p.publications.push(publication);
    track_alias
}

/// A contiguous object sent on the same Subgroup ID continues the already-open
/// stream instead of opening a new one.
#[test]
fn send_object_continues_contiguous_group() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);
    let track_alias = subscribed_track(&mut p);

    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5), &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, b"hello", &mut server.t).unwrap();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 1 }, 0, 5), &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 1 }, b"world", &mut server.t).unwrap();
    assert_eq!(server.session.in_progress_send_subgroups(track_alias).len(), 1, "both objects should share one subgroup");
}

/// A different `subgroup_id` opens its own stream even for an object that
/// would otherwise be contiguous.
#[test]
fn send_object_different_subgroup_id_opens_new_subgroup() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);
    let track_alias = subscribed_track(&mut p);

    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5), &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, b"hello", &mut server.t).unwrap();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 1 }, 1, 5), &mut server.t).unwrap();
    assert_eq!(server.session.in_progress_send_subgroups(track_alias).len(), 2, "a different subgroup_id should open its own stream");
}

/// An object sent on the same Subgroup ID after a numbering jump still
/// continues the existing stream (draft-19 §2.2: gaps within a Subgroup need
/// no confirmation), and a declared `prior_object_id_gap` is visible on the wire.
#[test]
fn send_object_with_confirmed_gap_continues_subgroup() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);
    let track_alias = subscribed_track(&mut p);

    let mut server = p.server();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5), &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, b"hello", &mut server.t).unwrap();
    // Objects 1-4 confirmed to never exist; object 5 continues the same subgroup.
    server.session.send_obj_hdr(track_alias, OutgoingObject { prior_object_id_gap: 4, ..OutgoingObject::new(Location { group: 0, object: 5 }, 0, 5) }, &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 5 }, b"world", &mut server.t).unwrap();
    assert_eq!(server.session.in_progress_send_subgroups(track_alias).len(), 1, "confirmed gap should still count as continuation");

    p.advance().unwrap();

    let mut client = p.client();
    let client_alias = *client.session.readable().first().unwrap();
    let (loc0, _subgroup_id0, hdr0) = expect_hdr(client.session.recv_new_obj_hdr(client_alias, &mut client.t));
    assert_eq!(hdr0.id(), 0);
    let mut buf = [0u8; 10];
    client.session.recv_obj_pld(client_alias, loc0, &mut buf, &mut client.t).unwrap();
    let (_loc1, _subgroup_id1, hdr1) = expect_hdr(client.session.recv_new_obj_hdr(client_alias, &mut client.t));
    assert_eq!(hdr1.id(), 5);
    assert_eq!(
        hdr1.properties().get_varint(moq_wire::property::PRIOR_OBJECT_ID_GAP),
        Some(4),
        "Prior Object ID Gap should be visible on the wire",
    );
}

/// A `prior_object_id_gap` larger than `object_id`, or one that would
/// retroactively cover an object_id already sent, is rejected.
#[test]
fn send_object_rejects_invalid_gap() {
    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);
    let track_alias = subscribed_track(&mut p);

    let mut server = p.server();
    let result = server.session.send_obj_hdr(track_alias, OutgoingObject { prior_object_id_gap: 4, ..OutgoingObject::new(Location { group: 0, object: 3 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::InvalidGap)), "gap larger than object_id: {result:?}");

    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 5 }, 0, 5), &mut server.t).unwrap();
    server.session.send_obj_pld(track_alias, Location { group: 0, object: 5 }, b"hello", &mut server.t).unwrap();
    // Object 5 was already sent; a gap claiming objects 3-4 don't exist is fine,
    // but one claiming 2-5 don't exist retroactively contradicts that.
    let result = server.session.send_obj_hdr(track_alias, OutgoingObject { prior_object_id_gap: 4, ..OutgoingObject::new(Location { group: 0, object: 6 }, 0, 5) }, &mut server.t);
    assert!(matches!(result, Err(Error::InvalidGap)), "gap covering an already-sent object_id: {result:?}");
}

fn test_webtransport_moq(version: Version) {
    let mut p = init_moq_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });

    let mut client = p.client();
    let _subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), SubscribeOptions::default(), &mut client.t).unwrap();
    p.advance().unwrap();

    let request = next_subscribe(p.server().session);
    let mut server = p.server();
    let publication = server.session.accept_request(request, SubscribeOk::default(), &mut server.t).unwrap();
    let track_alias = publication.track_alias();
    server.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, 5), &mut server.t).unwrap();
    let n = server.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, b"hello", &mut server.t).unwrap();
    assert_eq!(n, 5);
    p.advance().unwrap();

    let mut client = p.client();
    let track_alias = *client.session.readable().first().unwrap();
    let (location, _subgroup_id, _hdr) = expect_hdr(client.session.recv_new_obj_hdr(track_alias, &mut client.t));
    let mut buf = [0u8; 10];
    let n = client.session.recv_obj_pld(track_alias, location, &mut buf, &mut client.t).unwrap();
    assert_eq!(&buf[..n], b"hello");
}

/// Subscribes to two tracks with the given `subscriber_priority` values,
/// applies them via `set_out_track_priority` on accept (mirroring what a
/// real caller like moq_cli does), then streams a large object on each and
/// returns how many bytes of each had reached the client's application after
/// `rounds` round trips.
///
/// Round-stepping is manual (`quiche::test_utils::emit_flight`/`process_flight`
/// directly, not `MoqPipe::advance()`) because `advance()` fully drains both
/// sides every call — on a synchronous in-memory pipe the congestion window
/// ramps through slow start so quickly that contention becomes unobservable
/// after just a couple of full drains. Stepping one round trip at a time
/// keeps everything deterministic (no wall clock, no threads) while still
/// only letting through one congestion window's worth of data per round.
fn run_priority_round_trips(priority_a: u8, priority_b: u8, rounds: usize) -> (usize, usize) {
    use quiche::test_utils::{emit_flight, process_flight};

    let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
    let mut p = init_moq_pipe(config);

    let mut client = p.client();
    let sub_a = client.session.subscribe(&"n1--a".parse().unwrap(), SubscribeOptions { priority: priority_a, ..Default::default() }, &mut client.t).unwrap();
    let sub_b = client.session.subscribe(&"n1--b".parse().unwrap(), SubscribeOptions { priority: priority_b, ..Default::default() }, &mut client.t).unwrap();
    p.advance().unwrap();

    // `MoqTransportSession::poll` parses at most one control message per
    // readable-streams pass, so both SUBSCRIBEs arriving in the same flight
    // need multiple `poll_all()` calls to both land in the inbox.
    let mut alias_a = None;
    let mut alias_b = None;
    let mut publications = Vec::new();
    while alias_a.is_none() || alias_b.is_none() {
        p.server().poll_all();
        let Some(IncomingRequest::Subscribe(request)) = next_request(p.server().session) else {
            break;
        };
        let priority = request.message().subscriber_priority;
        let is_a = request.message().namespace_trackname.to_string() == "n1--a";
        let mut server = p.server();
        let publication = server.session.accept_request(request, SubscribeOk::default(), &mut server.t).unwrap();
        let alias = publication.track_alias();
        publications.push(publication);
        server.session.set_out_track_priority(alias, priority, false, &mut server.t);
        if is_a { alias_a = Some(alias); } else { alias_b = Some(alias); }
    }
    let (alias_a, alias_b) = (alias_a.unwrap(), alias_b.unwrap());
    p.advance().unwrap();
    // Same one-control-message-per-poll caveat applies to the two SUBSCRIBE_OKs
    // arriving back at the client.
    let mut resp_a = None;
    let mut resp_b = None;
    while resp_a.is_none() || resp_b.is_none() {
        p.client().poll_all();
        if resp_a.is_none() { resp_a = subscribe_response(p.client().session, sub_a.request_id()); }
        if resp_b.is_none() { resp_b = subscribe_response(p.client().session, sub_b.request_id()); }
    }
    resp_a.unwrap().unwrap();
    resp_b.unwrap().unwrap();

    // Writing a whole object in one call fails once the QUIC stream's available send capacity (tied to the congestion
    // window) is smaller than the object — exactly the regime this test
    // needs. Use `send_obj_hdr` + repeated `send_obj_pld` instead, topping up
    // each track's backlog by a fixed chunk every round (more than either
    // track could possibly flush in one round) so both tracks always have
    // pending data competing for the same send capacity, regardless of which
    // track we happen to top up first.
    const PAYLOAD_LEN: usize = 100_000;
    const CHUNK: usize = 30_000;
    let payload_a = vec![b'a'; PAYLOAD_LEN];
    let payload_b = vec![b'b'; PAYLOAD_LEN];
    let mut server = p.server();
    server.session.send_obj_hdr(alias_a, OutgoingObject::new(Location { group: 0, object: 0 }, 0, PAYLOAD_LEN), &mut server.t).unwrap();
    server.session.send_obj_hdr(alias_b, OutgoingObject::new(Location { group: 0, object: 0 }, 0, PAYLOAD_LEN), &mut server.t).unwrap();
    let mut sent_a = 0usize;
    let mut sent_b = 0usize;

    fn top_up(p: &mut MoqPipe, alias: TrackAlias, payload: &[u8], sent: &mut usize, want_more: usize) {
        let target = (*sent + want_more).min(payload.len());
        while *sent < target {
            let mut server = p.server();
            match server.session.send_obj_pld(alias, Location { group: 0, object: 0 }, &payload[*sent..target], &mut server.t) {
                Ok(n) if n > 0 => *sent += n,
                _ => break,
            }
        }
    }

    let mut received_a = 0usize;
    let mut received_b = 0usize;
    let mut loc_a: Option<moq_wire::Location> = None;
    let mut loc_b: Option<moq_wire::Location> = None;
    let mut buf = [0u8; 65536];

    for _ in 0..rounds {
        top_up(&mut p, alias_a, &payload_a, &mut sent_a, CHUNK);
        top_up(&mut p, alias_b, &payload_b, &mut sent_b, CHUNK);

        if let Ok(flight) = emit_flight(&mut p.pipe.server) {
            process_flight(&mut p.pipe.client, flight).unwrap();
        }
        if let Ok(flight) = emit_flight(&mut p.pipe.client) {
            process_flight(&mut p.pipe.server, flight).unwrap();
        }
        p.client().poll_all();

        let mut client = p.client();
        if loc_a.is_none() { loc_a = client.session.recv_new_obj_hdr(alias_a, &mut client.t).ok().and_then(|e| match e { RecvObjHdr::Header { location, .. } => Some(location), RecvObjHdr::SubgroupFinished { .. } | RecvObjHdr::SubgroupReset { .. } | RecvObjHdr::Datagram { .. } => None }); }
        if let Some(loc) = loc_a {
            let mut client = p.client();
            while let Ok(n) = client.session.recv_obj_pld(alias_a, loc, &mut buf, &mut client.t) {
                if n == 0 { break; }
                received_a += n;
            }
        }
        let mut client = p.client();
        if loc_b.is_none() { loc_b = client.session.recv_new_obj_hdr(alias_b, &mut client.t).ok().and_then(|e| match e { RecvObjHdr::Header { location, .. } => Some(location), RecvObjHdr::SubgroupFinished { .. } | RecvObjHdr::SubgroupReset { .. } | RecvObjHdr::Datagram { .. } => None }); }
        if let Some(loc) = loc_b {
            let mut client = p.client();
            while let Ok(n) = client.session.recv_obj_pld(alias_b, loc, &mut buf, &mut client.t) {
                if n == 0 { break; }
                received_b += n;
            }
        }
    }
    (received_a, received_b)
}

/// With differing subscriber priorities, the higher-priority (numerically
/// lower) track must receive substantially more bytes than the lower-priority
/// one after a limited number of congestion-window-constrained round trips.
#[test]
fn differing_priority_subscriptions_favor_higher_priority_track() {
    let (received_a, received_b) = run_priority_round_trips(0, 255, 4);
    assert!(
        received_a as f64 > received_b as f64 * 1.5,
        "expected priority-0 track to receive substantially more than priority-255 track after 4 rounds, got a={received_a} b={received_b}"
    );
}

/// With equal subscriber priority, neither track should be strongly favored:
/// bytes received after the same number of rounds should be comparable.
/// Tolerance is wider than it looks: quiche's pacing/congestion control
/// reads real `Instant`s even in this in-memory pipe, so a single round's
/// scheduling jitter under full test-suite parallelism can skew the
/// early-round split — this was observed to flake at a tighter bound.
#[test]
fn equal_priority_subscriptions_share_bandwidth_evenly() {
    let (received_a, received_b) = run_priority_round_trips(128, 128, 4);
    let (lo, hi) = if received_a < received_b { (received_a, received_b) } else { (received_b, received_a) };
    assert!(
        hi < lo * 3 + 1024,
        "expected comparable byte counts for equal-priority tracks after 4 rounds, got a={received_a} b={received_b}"
    );
}

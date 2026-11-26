//! Arbitrary messages from the peer close the session at worst, they never panic.

use super::*;
use crate::test_utils::{init_mock_pipe, subscribed_track, MockPipe, MockTransport};
use crate::transport::Transport;
use moq_wire::control_message::{FetchCancelMessage, FetchKind, FetchMessage, FetchRange, PublishNamespaceMessage, TrackStatusMessage, UnsubscribeMessage};
use moq_wire::{ToBytes, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18, MOQ_VERSION_DRAFT_19};

const ROUNDS: usize = 1_000;

/// xorshift64, deterministic so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Mostly small values, so request ids and track aliases hit live ones.
    fn body(&mut self, max_len: u64) -> Vec<u8> {
        let len = self.below(max_len);
        (0..len).map(|_| if self.below(4) == 0 { self.next() as u8 } else { self.below(8) as u8 }).collect()
    }
}

/// A well-formed message with small ids, so it meets live requests and tracks.
fn structured_message(rng: &mut Rng, version: Version) -> Option<Vec<u8>> {
    let request_id = RequestId(rng.below(4));
    let track_alias = TrackAlias(rng.below(4));
    let namespace: Namespace = "n1".parse().unwrap();
    let nt = NamespaceTrackname::new(vec![b"n1".to_vec()], vec![b't', b'0' + rng.below(3) as u8]).unwrap();
    let location = Location { group: rng.below(3), object: rng.below(3) };
    let filter = match rng.below(4) {
        0 => LocationFilter::NextGroupStart,
        1 => LocationFilter::LargestObject,
        2 => LocationFilter::AbsoluteStart { start: location },
        _ => LocationFilter::AbsoluteRange { start: location, end_group: rng.below(4) },
    };
    let subscribe = SubscribeMessage {
        track_alias: (!version.publisher_assigns_track_alias()).then_some(track_alias),
        subscriber_priority: rng.below(256) as u8,
        group_order: Some(GroupOrder::Descending),
        forward: rng.below(2) == 0,
        filter,
        ..SubscribeMessage::new(request_id, nt.clone())
    };
    let joining_request_id = RequestId(rng.below(4));
    let cm = match rng.below(17) {
        0 => ControlMessageEnum::Subscribe(subscribe),
        1 => ControlMessageEnum::SubscribeOk(SubscribeOkMessage::new(request_id, track_alias, Some(location), None)),
        2 => ControlMessageEnum::RequestError(RequestErrorMessage::new(request_id, RequestErrorCode::from(rng.below(4))).with_track_alias(Some(track_alias))),
        3 => ControlMessageEnum::PublishDone(PublishDoneMessage::new(request_id, PublishDoneStatus::from(rng.below(4)), rng.below(3))),
        4 => ControlMessageEnum::Unsubscribe(UnsubscribeMessage { request_id }),
        5 => ControlMessageEnum::RequestUpdate(RequestUpdateMessage { forward: Some(rng.below(2) == 0), ..RequestUpdateMessage::new(Some(request_id), RequestId(rng.below(4))) }),
        6 => ControlMessageEnum::MaxRequestId(MaxRequestIdMessage { max_request_id: RequestId(rng.below(16)) }),
        7 => ControlMessageEnum::RequestOk(RequestOkMessage::for_request_update(request_id, Some(location))),
        8 => ControlMessageEnum::Publish(PublishMessage::new(request_id, nt, track_alias, KeyValuePairs::new())),
        9 => ControlMessageEnum::PublishOk(PublishOkMessage::new(request_id)),
        10 => ControlMessageEnum::PublishNamespace(PublishNamespaceMessage::new(Some(request_id), namespace)),
        11 => ControlMessageEnum::PublishNamespaceDone(PublishNamespaceDoneMessage::new(request_id, namespace)),
        12 => ControlMessageEnum::TrackStatus(TrackStatusMessage { request_id: Some(request_id), namespace_trackname: nt }),
        13 => ControlMessageEnum::Fetch(FetchMessage::new(request_id, FetchKind::Standalone { namespace_trackname: nt, range: FetchRange { start: location, end: Location { group: location.group + 1, object: 0 } } })),
        14 => ControlMessageEnum::Fetch(FetchMessage::new(request_id, FetchKind::RelativeJoining { joining_request_id, groups_before: rng.below(3) })),
        15 => ControlMessageEnum::FetchCancel(FetchCancelMessage { request_id }),
        _ => ControlMessageEnum::RequestUpdate(RequestUpdateMessage {
            subscriber_priority: Some(rng.below(256) as u8),
            forward: Some(true),
            filter: Some(LocationFilter::AbsoluteStart { start: location }),
            ..RequestUpdateMessage::new(None, request_id)
        }),
    };
    let mut buf = vec![];
    cm.to_bytes(&mut version.writer(&mut buf), version).ok()?;
    Some(buf)
}

/// A control message of a known type with a matching length, so its body is decoded.
fn control_message(rng: &mut Rng, version: Version) -> Vec<u8> {
    if rng.below(8) != 0
        && let Some(message) = structured_message(rng, version)
    {
        return message;
    }
    let body = rng.body(48);
    let mut buf = vec![];
    let mut b = version.writer(&mut buf);
    b.varint(rng.below(0x20)).unwrap();
    let body = |b: &mut moq_wire::Writer| {
        b.bytes(&body);
        Ok::<_, moq_wire::EncodeError>(())
    };
    match version.message_length_is_fixed_u16() {
        true => b.with_u16_length(body),
        false => b.with_varint_length(body),
    }
    .unwrap();
    buf
}

/// A data stream of a subgroup type for `track_alias` or a fetch stream, the rest random.
fn data_stream(rng: &mut Rng, track_alias: TrackAlias) -> Vec<u8> {
    // a fetch stream for a small Request ID, or a subgroup stream
    let mut bytes = match rng.below(4) {
        0 => vec![moq_wire::fetch::FETCH_HEADER_TYPE as u8, rng.below(4) as u8],
        _ => vec![0x10 + rng.below(0x0E) as u8, track_alias.0 as u8],
    };
    bytes.extend(rng.body(64));
    bytes
}

/// A side and the stream a control message goes on: until draft 16 the control stream, the client's first bidi stream.
/// From draft 17 either control stream, the subscription's request stream or a new one.
fn control_stream<'a>(rng: &mut Rng, p: &'a mut MockPipe, version: Version) -> (&'a mut MockTransport, u64) {
    let (session, t) = if rng.below(2) == 0 { (&p.server, &mut p.server_t) } else { (&p.client, &mut p.client_t) };
    let stream_id = match rng.below(3) {
        _ if !version.stream_per_request() => 0,
        0 => session.control_channel.out_stream().unwrap(),
        // `subscribed_track`'s SUBSCRIBE opened the client's first bidi stream
        1 => 0,
        _ => t.open_bidi_stream().unwrap(),
    };
    (t, stream_id)
}

/// Everything the application does with what arrived.
fn drive(p: &mut MockPipe, track_alias: TrackAlias) {
    for (session, t) in [(&mut p.client, &mut p.client_t), (&mut p.server, &mut p.server_t)] {
        // dropping requests rejects them
        while session.next_event().is_some() {}
        let mut buf = [0u8; 64];
        while let Ok(read) = session.recv_new_obj_hdr(track_alias, t) {
            if let RecvObjHdr::Header { location, .. } = read {
                let _ = session.recv_obj_pld(track_alias, location, &mut buf, t);
            }
        }
    }
    p.advance();
}

#[test]
fn arbitrary_peer_messages_never_panic() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for version in [MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18, MOQ_VERSION_DRAFT_19] {
        for _ in 0..ROUNDS {
            let mut p = init_mock_pipe(Config { version_negotiation: VersionNegotiation::only(version), ..Config::default() });
            let track_alias = subscribed_track(&mut p);
            assert!(track_alias.0 < 64, "single byte varint");
            for _ in 0..1 + rng.below(3) {
                let message = control_message(&mut rng, version);
                let (t, stream_id) = control_stream(&mut rng, &mut p, version);
                t.send_stream(stream_id, &message, false).unwrap();
            }
            for _ in 0..rng.below(3) {
                let stream_id = p.server_t.open_uni_stream().unwrap();
                p.server_t.send_stream(stream_id, &data_stream(&mut rng, track_alias), rng.below(2) == 0).unwrap();
            }
            p.advance();
            drive(&mut p, track_alias);
            drive(&mut p, track_alias);
        }
    }
}

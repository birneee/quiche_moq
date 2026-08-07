use crate::test_utils::{init_moq_pipe, MoqPipe};
use crate::{Config, Error, VersionNegotiation};
use moq_wire::TrackAlias;
use moq_wire::control_message::subscribe::FilterType;
use moq_wire::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, Version};
use moq_wire::location::Location;
use moq_wire::Parameters;
use std::time::{Duration, Instant};

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
    //test_webtransport_moq_lite01: MOQ_VERSION_LITE_01_BY_KIXELATED,
}

#[test]
fn delivery_timeout() {
    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);
    let mut p = init_moq_pipe(config);

    // Subscriber requests with a 5000ms delivery_timeout.
    let sub_request_id = p.client().subscribe(&"n1--t1".parse().unwrap(), None, Some(5000), FilterType::LargestObject).unwrap();
    p.advance().unwrap();

    // Publisher sees the delivery_timeout in the SUBSCRIBE.
    let track_alias = {
        let mut s = p.server();
        let (&request_id, subscription) = s.pending_received_subscriptions().iter().next().unwrap();
        assert_eq!(subscription.delivery_timeout(), Some(5000));
        s.accept_subscription(request_id, None, None)
    };
    p.advance().unwrap();

    p.client().poll_subscribe_response(sub_request_id).unwrap().unwrap();

    // A fresh object (received now) is within the timeout and sends successfully.
    let now = Instant::now();
    p.server().send_obj_hdr(5, track_alias, Some(now)).unwrap();

    // An old object exceeds the timeout and returns ObjectExpired.
    let old = Instant::now() - Duration::from_secs(10);
    let result = p.server().send_obj_hdr(5, track_alias, Some(old));
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // Subscriber sends REQUEST_UPDATE increasing the delivery_timeout to 60 seconds.
    p.client().request_update_delivery_timeout(sub_request_id, Some(60_000)).unwrap();
    p.advance().unwrap();

    // After updating, the 10-second-old object is within the new 60s timeout.
    p.server().send_obj_hdr(5, track_alias, Some(old)).unwrap();
}

#[test]
fn max_cache_duration() {
    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);
    let mut p = init_moq_pipe(config);

    let sub_request_id = p.client().subscribe(&"n1--t1".parse().unwrap(), None, None, FilterType::LargestObject).unwrap();
    p.advance().unwrap();

    let (&request_id, _) = p.server().pending_received_subscriptions().iter().next().unwrap();
    // Accept with a 5000ms max_cache_duration.
    let track_alias = p.server().accept_subscription(request_id, Some(Location { group: 1, object: 0 }), Some(5000));
    p.advance().unwrap();

    // Subscriber receives max_cache_duration in SUBSCRIBE_OK.
    let (_, ok_msg) = p.client().poll_subscribe_response(sub_request_id).unwrap().unwrap();
    assert_eq!(ok_msg.max_cache_duration(), Some(5000));
    assert_eq!(ok_msg.largest_location(), Some(Location { group: 1, object: 0 }));

    // A fresh object is within max_cache_duration and sends successfully.
    let now = Instant::now();
    p.server().send_obj_hdr(5, track_alias, Some(now)).unwrap();

    // An old object exceeds max_cache_duration and returns ObjectExpired.
    let old = Instant::now() - Duration::from_secs(10);
    let result = p.server().send_obj_hdr(5, track_alias, Some(old));
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);
}

/// Models `moq_cli::node`'s relay delivery_timeout aggregation (Phase 4): a
/// relay subscribes upstream with the max `delivery_timeout` across all of
/// its downstream subscribers, and raises that subscription's
/// delivery_timeout via REQUEST_UPDATE if a later subscriber needs more
/// leniency than the relay originally requested. This exercises the exact
/// `subscribe` + `request_update_delivery_timeout` sequence the relay's
/// aggregation logic performs against the upstream publisher.
#[test]
fn relay_raises_upstream_delivery_timeout_via_request_update() {
    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);
    let mut p = init_moq_pipe(config);

    // Relay subscribes upstream with its first (only) downstream
    // subscriber's delivery_timeout: 1000ms.
    let req = p.client().subscribe(&"n1--t1".parse().unwrap(), None, Some(1_000), FilterType::LargestObject).unwrap();
    p.advance().unwrap();
    let track_alias = {
        let mut s = p.server();
        let (&request_id, _) = s.pending_received_subscriptions().iter().next().unwrap();
        s.accept_subscription(request_id, None, None)
    };
    p.advance().unwrap();
    p.client().poll_subscribe_response(req).unwrap().unwrap();

    // A 4-second-old object exceeds the 1000ms timeout.
    let old = Instant::now() - Duration::from_secs(4);
    let result = p.server().send_obj_hdr(5, track_alias, Some(old));
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // A second downstream subscriber joins wanting a 5000ms timeout. The
    // relay's aggregate is now max(1000, 5000) = 5000, raised upstream via
    // REQUEST_UPDATE rather than a new SUBSCRIBE.
    p.client().request_update_delivery_timeout(req, Some(5_000)).unwrap();
    p.advance().unwrap();

    // The same 4-second-old object is now within the raised 5000ms timeout.
    p.server().send_obj_hdr(5, track_alias, Some(old)).unwrap();
}

/// Models `moq_cli::node`'s relay max_cache_duration propagation (Phase 4):
/// the relay reads `max_cache_duration` from its own SUBSCRIBE_OK from the
/// upstream publisher and passes that same value to `accept_subscription`
/// for every downstream subscriber, so their `OutTrack` enforces the same
/// expiry the upstream publisher promised. Uses two independent pipes (the
/// relay's upstream-facing session and its downstream-facing session are
/// genuinely separate `MoqTransportSession`s over separate connections, just
/// like in moq_cli) and threads the value between them exactly as Phase 4 does.
#[test]
fn relay_propagates_max_cache_duration_from_upstream_to_downstream() {
    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);

    // Pipe A: origin publisher (server) <-> relay's upstream-facing session (client).
    let mut upstream_pipe = init_moq_pipe(config.clone());
    // Pipe B: relay's downstream-facing session (server) <-> subscriber (client).
    let mut downstream_pipe = init_moq_pipe(config);

    let down_req = downstream_pipe.client().subscribe(&"n1--t1".parse().unwrap(), None, None, FilterType::LargestObject).unwrap();
    downstream_pipe.advance().unwrap();

    let up_req = upstream_pipe.client().subscribe(&"n1--t1".parse().unwrap(), None, None, FilterType::LargestObject).unwrap();
    upstream_pipe.advance().unwrap();

    // Origin accepts with an 8-second max_cache_duration.
    let up_accept_request_id = *upstream_pipe.server().pending_received_subscriptions().keys().next().unwrap();
    upstream_pipe.server().accept_subscription(up_accept_request_id, None, Some(8_000));
    upstream_pipe.advance().unwrap();
    let (_, ok_msg) = upstream_pipe.client().poll_subscribe_response(up_req).unwrap().unwrap();
    let max_cache_duration = ok_msg.max_cache_duration();
    assert_eq!(max_cache_duration, Some(8_000));

    // Relay propagates that same value to its own downstream subscriber's SUBSCRIBE_OK.
    let relay_alias = {
        let mut s = downstream_pipe.server();
        let (&request_id, _) = s.pending_received_subscriptions().iter().next().unwrap();
        s.accept_subscription(request_id, None, max_cache_duration)
    };
    downstream_pipe.advance().unwrap();
    downstream_pipe.client().poll_subscribe_response(down_req).unwrap().unwrap();

    // A 9-second-old object exceeds the propagated 8s max_cache_duration...
    let too_old = Instant::now() - Duration::from_secs(9);
    let result = downstream_pipe.server().send_obj_hdr(5, relay_alias, Some(too_old));
    assert!(matches!(result, Err(Error::ObjectExpired)), "expected ObjectExpired, got {:?}", result);

    // ...while a 7-second-old object is still within it.
    let within = Instant::now() - Duration::from_secs(7);
    downstream_pipe.server().send_obj_hdr(5, relay_alias, Some(within)).unwrap();
}

/// PUBLISH (0x1D) flow: publisher sends PUBLISH, subscriber accepts with PUBLISH_OK,
/// publisher streams an object, subscriber reads it.
#[test]
fn publish_track() {
    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);
    let mut p = init_moq_pipe(config);

    // Publisher (server side) sends PUBLISH and immediately streams an object.
    let (pub_request_id, track_alias) = p.server()
        .publish_track("n1--t1".parse().unwrap(), Parameters(vec![]))
        .unwrap();
    p.server().send_obj(b"hello", track_alias).unwrap();
    p.advance().unwrap();

    // Subscriber (client side) receives the PUBLISH and accepts it.
    p.client().post_handle();
    let result = p.client().poll_all();
    assert!(result.pending_track_publishes());
    let (&request_id, _cm) = p.client().pending_received_track_publishes().iter().next().unwrap();
    p.client().accept_track_publish(request_id, Parameters(vec![]));
    p.client().post_handle();
    p.advance().unwrap();

    // Publisher receives PUBLISH_OK.
    p.server().post_handle();
    let result = p.server().poll_all();
    assert!(result.pending_subscribe_responses() || !result.pending_track_publishes());
    let resp = p.server().poll_publish_response(pub_request_id).unwrap();
    assert!(resp.is_ok());

    // Subscriber reads the object that arrived before PUBLISH_OK.
    let track_alias = *p.client().readable().first().unwrap();
    let _hdr = p.client().read_obj_hdr(track_alias).unwrap();
    let mut buf = [0u8; 10];
    let n = p.client().read_obj_pld(&mut buf, track_alias).unwrap();
    assert_eq!(&buf[..n], b"hello");
}

fn test_webtransport_moq(version: Version) {
    let mut config: Config = Default::default();
    // draft >= 15 negotiates via ALPN/WT-Available-Protocols, not
    // CLIENT_SETUP's version list — see `VersionNegotiation`.
    config.version_negotiation = if version >= MOQ_VERSION_DRAFT_15 {
        VersionNegotiation::Pinned(version)
    } else {
        VersionNegotiation::InBand { candidates: vec![version] }
    };
    let mut p = init_moq_pipe(config);

    p.client().subscribe(&"n1--t1".parse().unwrap(), None, None, FilterType::LargestObject).unwrap();
    p.advance().unwrap();

    let (&request_id, _) = p.server().pending_received_subscriptions().iter().next().unwrap();
    let track_alias = p.server().accept_subscription(request_id, None, None);
    p.server().send_obj(b"hello", track_alias).unwrap();
    p.advance().unwrap();

    let track_alias = *p.client().readable().first().unwrap();
    let _hdr = p.client().read_obj_hdr(track_alias).unwrap();
    let mut buf = [0u8; 10];
    let n = p.client().read_obj_pld(&mut buf, track_alias).unwrap();
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

    let mut config: Config = Default::default();
    config.version_negotiation = VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16);
    let mut p = init_moq_pipe(config);

    let req_a = p.client().subscribe(&"n1--a".parse().unwrap(), Some(priority_a), None, FilterType::LargestObject).unwrap();
    let req_b = p.client().subscribe(&"n1--b".parse().unwrap(), Some(priority_b), None, FilterType::LargestObject).unwrap();
    p.advance().unwrap();

    // `MoqTransportSession::poll` parses at most one control message per
    // readable-streams pass, so both SUBSCRIBEs arriving in the same flight
    // need multiple `poll_all()` calls to both land in the inbox.
    let mut alias_a = None;
    let mut alias_b = None;
    p.server().post_handle();
    while alias_a.is_none() || alias_b.is_none() {
        p.server().poll_all();
        let server = p.server();
        let Some((&request_id, cm)) = server.pending_received_subscriptions().iter().next() else {
            p.server().post_handle();
            break;
        };
        let priority = cm.subscriber_priority;
        let is_a = cm.namespace_trackname.to_string() == "n1--a";
        drop(server);
        let alias = p.server().accept_subscription(request_id, None, None);
        p.server().set_out_track_priority(alias, priority, false);
        if is_a { alias_a = Some(alias); } else { alias_b = Some(alias); }
        p.server().post_handle();
    }
    let (alias_a, alias_b) = (alias_a.unwrap(), alias_b.unwrap());
    p.advance().unwrap();
    // Same one-control-message-per-poll caveat applies to the two SUBSCRIBE_OKs
    // arriving back at the client.
    let mut resp_a = None;
    let mut resp_b = None;
    p.client().post_handle();
    while resp_a.is_none() || resp_b.is_none() {
        p.client().poll_all();
        if resp_a.is_none() { resp_a = p.client().poll_subscribe_response(req_a); }
        if resp_b.is_none() { resp_b = p.client().poll_subscribe_response(req_b); }
        p.client().post_handle();
    }
    resp_a.unwrap().unwrap();
    resp_b.unwrap().unwrap();

    // `send_obj` requires writing the whole buffer in one call, which fails
    // once the QUIC stream's available send capacity (tied to the congestion
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
    p.server().send_obj_hdr(PAYLOAD_LEN, alias_a, None).unwrap();
    p.server().send_obj_hdr(PAYLOAD_LEN, alias_b, None).unwrap();
    let mut sent_a = 0usize;
    let mut sent_b = 0usize;

    fn top_up(p: &mut MoqPipe, alias: TrackAlias, payload: &[u8], sent: &mut usize, want_more: usize) {
        let target = (*sent + want_more).min(payload.len());
        while *sent < target {
            match p.server().send_obj_pld(&payload[*sent..target], alias) {
                Ok(n) if n > 0 => *sent += n,
                _ => break,
            }
        }
    }

    let mut received_a = 0usize;
    let mut received_b = 0usize;
    let mut hdr_a = false;
    let mut hdr_b = false;
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

        if !hdr_a && p.client().read_obj_hdr(alias_a).is_ok() { hdr_a = true; }
        if hdr_a {
            while let Ok(n) = p.client().read_obj_pld(&mut buf, alias_a) {
                if n == 0 { break; }
                received_a += n;
            }
        }
        if !hdr_b && p.client().read_obj_hdr(alias_b).is_ok() { hdr_b = true; }
        if hdr_b {
            while let Ok(n) = p.client().read_obj_pld(&mut buf, alias_b) {
                if n == 0 { break; }
                received_b += n;
            }
        }
        p.client().post_handle();
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
#[test]
fn equal_priority_subscriptions_share_bandwidth_evenly() {
    let (received_a, received_b) = run_priority_round_trips(128, 128, 4);
    let (lo, hi) = if received_a < received_b { (received_a, received_b) } else { (received_b, received_a) };
    assert!(
        hi < lo * 2 + 1024,
        "expected comparable byte counts for equal-priority tracks after 4 rounds, got a={received_a} b={received_b}"
    );
}

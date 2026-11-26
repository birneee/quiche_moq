mod connection;
mod error;
pub mod error_code;
mod pending_request;
mod pending_response;
mod protocol_negotiation;
mod session;
mod stream;
pub mod test_utils;
#[cfg(test)]
mod fuzz_tests;

pub use crate::connection::Connection;
pub use crate::stream::is_uni;
pub use crate::error::Error;
pub use crate::error::Result;

pub type SessionId = u64;

/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-02.html#name-unidirectional-streams
pub const WEBTRANSPORT_UNI_STREAM_TYPE_ID: u64 = 0x54;
/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-02.html#name-bidirectional-streams
pub const WEBTRANSPORT_BIDI_STREAM_TYPE_ID: u64 = 0x41;

/// https://www.rfc-editor.org/rfc/rfc9000#name-variable-length-integer-enc
const MAX_VARINT_LEN: usize = 8;

/// The upgrade token until draft 14 (RFC 8441).
const PROTOCOL_HEADER_WEBTRANSPORT: &[u8] = b"webtransport";
/// The upgrade token from draft 15 (draft-ietf-webtrans-http3-16 §3.2).
const PROTOCOL_HEADER_WEBTRANSPORT_H3: &[u8] = b"webtransport-h3";

/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-02.html#name-http-3-settings-parameter-r
/// from draft 0 to draft 6
pub const ENABLE_WEBTRANSPORT_H3_SETTINGS_PARAMETER_ID: u64 = 0x2b603742;

// from draft 4 to draft 5
pub const WEBTRANSPORT_MAX_SESSIONS_SETTINGS_PARAMETER_ID_DRAFT_4_5: u64 = 0x2b603743;

// from draft 6
pub const WEBTRANSPORT_MAX_SESSIONS_SETTINGS_PARAMETER_ID_DRAFT_6: u64 = 0x3c48d522;

/// from draft 7 to draft 12
pub const WEBTRANSPORT_MAX_SESSIONS_SETTINGS_PARAMETER_ID_DRAFT_7_12: u64 = 0xc671706a;

/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-13.html#name-http-3-settings-parameter-r
/// from draft 13
pub const WT_MAX_SESSIONS_H3_SETTINGS_PARAMETER_ID: u64 = 0x14e9cd29;


/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-14.html#name-capsule-types
pub const WT_CLOSE_SESSION: u64 = 0x2843;

/// SETTINGS_WT_ENABLED of draft-ietf-webtrans-http3-16 §3.1, each draft has its own codepoint (§7.1).
pub const WT_ENABLED_DRAFT_16_H3_SETTINGS_PARAMETER_ID: u64 = 0x2c7cf000;

/// Resets the streams of a terminated session, a protocol error code (draft-ietf-webtrans-http3-16 §6).
pub const WT_SESSION_GONE: u64 = 0x170d7b68;
/// Rejects a stream beyond the buffered ones of sessions not established yet (§4.6).
pub const WT_BUFFERED_STREAM_REJECTED: u64 = 0x3994bd84;
/// Closes the connection on a malformed HTTP Datagram (RFC 9297 §5.2).
pub const H3_DATAGRAM_ERROR: u64 = 0x33;

/// Configure H3 for WebTransport: draft 16, and the settings of drafts 2 and 13 for older peers.
// TODO SETTINGS_WT_ENABLED of draft 17 (0x20b8933b), its wire format differs only in what we don't do
pub fn configure_h3(c: &mut quiche::h3::Config) -> core::result::Result<(), quiche::h3::Error> {
    c.enable_extended_connect(true);
    c.enable_webtransport_streams(true);
    c.set_additional_settings(vec![
        (ENABLE_WEBTRANSPORT_H3_SETTINGS_PARAMETER_ID, 1),
        (WT_MAX_SESSIONS_H3_SETTINGS_PARAMETER_ID, 1),
        (WT_ENABLED_DRAFT_16_H3_SETTINGS_PARAMETER_ID, 1),
    ])?;
    Ok(())
}

/// Whether the peer speaks draft 16: it sent SETTINGS_WT_ENABLED (§7.1).
pub(crate) fn draft_16_by_peer(c: &quiche::h3::Connection) -> bool {
    c.peer_settings_raw().into_iter().flatten().any(|&setting| setting == (WT_ENABLED_DRAFT_16_H3_SETTINGS_PARAMETER_ID, 1))
}

/// Check if the H3 peer has enabled WebTransport.
/// Returns `false` if the settings have not been received yet.
pub fn webtransport_enabled_by_server(c: &quiche::h3::Connection) -> bool {
    if !c.extended_connect_enabled_by_peer() {
        return false;
    }

    for &(key, value) in c.peer_settings_raw().into_iter().flatten() {
        match key {
            ENABLE_WEBTRANSPORT_H3_SETTINGS_PARAMETER_ID | WT_ENABLED_DRAFT_16_H3_SETTINGS_PARAMETER_ID => {
                if value == 1 {
                    return true;
                }
            }
            WT_MAX_SESSIONS_H3_SETTINGS_PARAMETER_ID => {
                if value > 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::test_utils::{_init_h3_pipe, _init_webtransport_pipe};
    use crate::{Error, WT_CLOSE_SESSION};
    use quiche::h3::NameValue;
    use quiche::h3;

    #[test]
    fn send_on_unidirectional_stream() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

        const MSG: &[u8] = b"hello";

        let wt_stream_id = c_wt
            .open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false)
            .unwrap();
        c_wt.stream_send(wt_stream_id, &mut c_h3, &mut pipe.client, MSG, false)
            .unwrap();

        pipe.advance().unwrap();

        assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
        s_wt.poll(&mut s_h3, &mut pipe.server);
        assert_eq!(s_wt.readable_streams(wt_session_id), &[wt_stream_id]);
        let mut b = [0u8; 100];
        let len = s_wt.recv_stream(wt_stream_id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b).unwrap();
        assert_eq!(&b[..len], MSG);
    }

    /// An HTTP Datagram starts with its session's Quarter Stream ID (RFC 9297 §2.1).
    #[test]
    fn datagrams_carry_their_session() {
        let (mut pipe, c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let max = c_wt.max_datagram_payload(wt_session_id, &c_h3, &pipe.client).unwrap();
        c_wt.send_datagram(wt_session_id, &mut pipe.client, b"hello").unwrap();
        assert!(matches!(c_wt.send_datagram(wt_session_id, &mut pipe.client, &vec![0; max + 1]), Err(Error::Done)));
        pipe.advance().unwrap();
        s_wt.poll(&mut s_h3, &mut pipe.server);
        assert_eq!(s_wt.recv_datagram(wt_session_id).as_deref(), Some(&b"hello"[..]));
        assert_eq!(s_wt.recv_datagram(wt_session_id), None);
    }

    #[test]
    fn quarter_stream_id_beyond_2_60_closes_the_connection() {
        let (mut pipe, _c_h3, _c_wt, mut s_h3, mut s_wt, _) = _init_webtransport_pipe();
        pipe.client.dgram_send(&[0xff; 8]).unwrap();
        pipe.advance().unwrap();
        s_wt.poll(&mut s_h3, &mut pipe.server);
        assert!(pipe.server.local_error().is_some_and(|e| e.error_code == crate::H3_DATAGRAM_ERROR));
    }

    /// The RESET_STREAM carries the remapped code, `recv_stream` reports the application's.
    #[test]
    fn reset_code_is_remapped() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let mut b = [0u8; 100];
        let open = |pipe: &mut quiche::test_utils::Pipe, c_h3: &mut h3::Connection, c_wt: &mut crate::Connection, s_h3: &mut h3::Connection, s_wt: &mut crate::Connection| {
            let id = c_wt.open_stream(wt_session_id, c_h3, &mut pipe.client, false).unwrap();
            c_wt.stream_send(id, c_h3, &mut pipe.client, b"x", false).unwrap();
            pipe.advance().unwrap();
            assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
            s_wt.poll(s_h3, &mut pipe.server);
            id
        };

        let raw_id = open(&mut pipe, &mut c_h3, &mut c_wt, &mut s_h3, &mut s_wt);
        c_wt.shutdown_stream(raw_id, quiche::Shutdown::Write, 7, &mut c_h3, &mut pipe.client).unwrap();
        pipe.advance().unwrap();
        let raw = loop {
            match pipe.server.stream_recv(raw_id, &mut b) {
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert_eq!(raw, quiche::Error::StreamReset(crate::error_code::to_http3(7)));

        let id = open(&mut pipe, &mut c_h3, &mut c_wt, &mut s_h3, &mut s_wt);
        assert_eq!(s_wt.recv_stream(id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b).unwrap(), 1);
        c_wt.shutdown_stream(id, quiche::Shutdown::Write, 7, &mut c_h3, &mut pipe.client).unwrap();
        pipe.advance().unwrap();
        let result = s_wt.recv_stream(id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b);
        assert!(matches!(result, Err(Error::StreamReset(7))), "{result:?}");
    }

    /// `recv_stream` must remove the stream from `Connection::streams` as
    /// soon as it reports `Fin` — no periodic GC sweep needed to reclaim it.
    #[test]
    fn stream_is_removed_from_map_once_fin_is_observed() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

        let wt_stream_id = c_wt
            .open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false)
            .unwrap();
        c_wt.stream_send(wt_stream_id, &mut c_h3, &mut pipe.client, b"bye", true).unwrap();

        pipe.advance().unwrap();

        assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
        s_wt.poll(&mut s_h3, &mut pipe.server);

        let mut b = [0u8; 100];
        let len = s_wt.recv_stream(wt_stream_id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b).unwrap();
        assert_eq!(&b[..len], b"bye");
        assert!(s_wt.streams.contains_key(&wt_stream_id), "not yet removed: this call returned the data, not Fin");

        let result = s_wt.recv_stream(wt_stream_id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b);
        assert!(matches!(result, Err(Error::Fin)), "expected Fin: {result:?}");
        assert!(!s_wt.streams.contains_key(&wt_stream_id), "stream must be removed once Fin is observed");
    }

    #[test]
    fn rejected_connect_closes_connection() {
        let (mut pipe, mut c_h3, mut s_h3) = _init_h3_pipe();
        let mut c_wt = crate::Connection::new(false);
        let session_id = c_wt.connect_session(&mut c_h3, &mut pipe.client, "https://example.org/".parse().unwrap(), &[]).unwrap();
        pipe.advance().unwrap();
        assert!(matches!(s_h3.poll(&mut pipe.server), Ok((_, h3::Event::Headers { .. }))));
        s_h3.send_response(&mut pipe.server, session_id, &[h3::Header::new(b":status", b"404")], false).unwrap();
        pipe.advance().unwrap();

        c_wt.poll_h3(&mut c_h3, &mut pipe.client);
        assert!(!c_wt.established(session_id));
        let error = pipe.client.local_error().expect("connection closed");
        assert_eq!(error.error_code, h3::WireErrorCode::GeneralProtocolError as u64);
    }

    /// draft-ietf-webtrans-http3 §6: the CONNECT stream's end ends the session.
    #[test]
    fn finished_connect_stream_closes_connection() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, _s_wt, wt_session_id) = _init_webtransport_pipe();
        s_h3.send_body(&mut pipe.server, wt_session_id, b"", true).unwrap();
        pipe.advance().unwrap();

        c_wt.poll_h3(&mut c_h3, &mut pipe.client);
        let error = pipe.client.local_error().expect("connection closed");
        assert_eq!(error.error_code, h3::WireErrorCode::NoError as u64);
    }

    /// A stream that ends within its session ID is dropped.
    #[test]
    fn stream_ending_within_session_id_is_dropped() {
        let (mut pipe, mut c_h3, _c_wt, mut s_h3, mut s_wt, _wt_session_id) = _init_webtransport_pipe();
        // the stream type alone, then half of a two byte session ID
        let stream_id = c_h3.open_webtransport_stream(&mut pipe.client, false).unwrap();
        pipe.client.stream_send(stream_id, &[0x40], true).unwrap();
        pipe.advance().unwrap();

        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server);
        assert!(s_wt.streams.is_empty());
    }

    /// A client asks a draft 16 server with its upgrade token, without the header of draft 2
    /// (draft-ietf-webtrans-http3-16 §3.2).
    #[test]
    fn connect_uses_the_token_of_draft_16() {
        let (mut pipe, mut c_h3, mut s_h3) = _init_h3_pipe();
        let mut c_wt = crate::Connection::new(false);
        c_wt.connect_session(&mut c_h3, &mut pipe.client, "https://example.org/".parse().unwrap(), &[]).unwrap();
        pipe.advance().unwrap();
        let Ok((_, h3::Event::Headers { list, .. })) = s_h3.poll(&mut pipe.server) else { panic!("no CONNECT") };
        assert!(list.iter().any(|h| h.name() == b":protocol" && h.value() == b"webtransport-h3"));
        assert!(!list.iter().any(|h| h.name() == b"sec-webtransport-http3-draft02"));
    }

    /// Without flow control a connection carries one session, a second CONNECT is rejected (§5.1).
    #[test]
    fn second_session_is_rejected() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, _wt_session_id) = _init_webtransport_pipe();
        let second = c_wt.connect_session(&mut c_h3, &mut pipe.client, "https://example.org/".parse().unwrap(), &[]).unwrap();
        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        pipe.advance().unwrap();
        assert!(!s_wt.established(second));
        let mut buf = [0u8; 16];
        assert_eq!(pipe.client.stream_recv(second, &mut buf), Err(quiche::Error::StreamReset(h3::WireErrorCode::RequestRejected as u64)));
    }

    /// Closing a session resets its streams with WT_SESSION_GONE (§6).
    #[test]
    fn close_session_resets_its_streams() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let stream_id = c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, true).unwrap();
        c_wt.stream_send(stream_id, &mut c_h3, &mut pipe.client, b"x", false).unwrap();
        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server);
        s_wt.close_session(wt_session_id, 0, "", &mut pipe.server, &mut s_h3).unwrap();
        pipe.advance().unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(pipe.client.stream_recv(stream_id, &mut buf), Err(quiche::Error::StreamReset(crate::WT_SESSION_GONE)));
    }

    /// The peer's WT_CLOSE_SESSION ends the session's streams, sending on one fails as stopped (§6).
    #[test]
    fn send_after_peer_close_fails_as_stopped() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let stream_id = c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false).unwrap();
        s_wt.close_session(wt_session_id, 0, "", &mut pipe.server, &mut s_h3).unwrap();
        pipe.advance().unwrap();
        c_wt.poll_h3(&mut c_h3, &mut pipe.client);
        let sent = c_wt.stream_send(stream_id, &mut c_h3, &mut pipe.client, b"x", false);
        assert!(matches!(sent, Err(Error::StreamStopped(crate::WT_SESSION_GONE))), "{sent:?}");
    }

    /// WT_CLOSE_SESSION is followed by a FIN, which ends the session at the peer.
    #[test]
    fn close_session_ends_session_at_peer() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let long_message = "é".repeat(600);
        s_wt.close_session(wt_session_id, 7, &long_message, &mut pipe.server, &mut s_h3).unwrap();
        s_wt.close_session(wt_session_id, 7, "again", &mut pipe.server, &mut s_h3).unwrap();
        pipe.advance().unwrap();

        c_wt.poll_h3(&mut c_h3, &mut pipe.client);
        assert_eq!(c_wt.peer_close(wt_session_id), Some((7, &long_message[..1024])));
        let error = pipe.client.local_error().expect("connection closed");
        assert_eq!(error.error_code, h3::WireErrorCode::NoError as u64);
    }

    #[test]
    fn no_stream_opens_on_a_closed_session() {
        let (mut pipe, mut c_h3, mut c_wt, _s_h3, _s_wt, wt_session_id) = _init_webtransport_pipe();
        c_wt.close_session(wt_session_id, 0, "", &mut pipe.client, &mut c_h3).unwrap();
        assert!(matches!(c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false), Err(Error::MissingSessionID)));
    }

    /// The server's connection error after the client sent `capsules` on the CONNECT stream.
    fn server_error_after_capsules(capsules: &[(u64, &[u8])]) -> Option<u64> {
        let (mut pipe, mut c_h3, _c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        for &(ty, value) in capsules {
            let mut buf = [0u8; 64];
            let len = http_capsule::Capsule::new(ty, value.to_vec()).encode(&mut buf).unwrap();
            c_h3.send_body(&mut pipe.client, wt_session_id, &buf[..len], false).unwrap();
        }
        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        if capsules.iter().any(|&(ty, _)| ty == WT_CLOSE_SESSION) {
            assert_eq!(s_wt.peer_close(wt_session_id).map(|(code, _)| code), Some(9));
        }
        pipe.server.local_error().map(|e| e.error_code)
    }

    #[test]
    fn peer_close_is_recorded_after_unknown_capsules() {
        assert_eq!(server_error_after_capsules(&[(0x17, b"x"), (WT_CLOSE_SESSION, b"\0\0\0\x09bye")]), None);
    }

    #[test]
    fn capsule_after_peer_close_closes_connection() {
        let error = server_error_after_capsules(&[(WT_CLOSE_SESSION, b"\0\0\0\x09"), (0x17, b"")]);
        assert_eq!(error, Some(h3::WireErrorCode::MessageError as u64));
    }

    #[test]
    fn malformed_peer_close_closes_connection() {
        let (mut pipe, mut c_h3, _c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let mut buf = [0u8; 16];
        let len = http_capsule::Capsule::new(WT_CLOSE_SESSION, vec![0, 9]).encode(&mut buf).unwrap();
        c_h3.send_body(&mut pipe.client, wt_session_id, &buf[..len], false).unwrap();
        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        assert_eq!(pipe.server.local_error().map(|e| e.error_code), Some(h3::WireErrorCode::MessageError as u64));
    }

    #[test]
    fn close_message_is_truncated_at_char_boundary() {
        let message = "é".repeat(600);
        let truncated = crate::connection::truncate_utf8(&message, 1024);
        assert_eq!(truncated.len(), 1024);
        assert_eq!(crate::connection::truncate_utf8("ab", 1024), "ab");
        assert_eq!(crate::connection::truncate_utf8("aé", 2), "a");
    }

    /// Regression test for `push_sample_groups_do_not_leak_streams` /
    /// `relay_does_not_panic_forwarding_canceled_object`: many concurrent
    /// uni streams, each read incrementally across several `poll()` cycles
    /// (mirroring `moq_core`'s header-then-payload reads), with some
    /// cancelled mid-flight via a raw QUIC reset (a peer that bypasses
    /// `wt`/h3 entirely). `s_wt.poll()` must never panic.
    #[test]
    fn many_concurrent_streams_partial_reads_and_resets() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

        const NUM_STREAMS: usize = 10;
        let mut stream_ids = Vec::new();
        for i in 0..NUM_STREAMS {
            let stream_id = c_wt
                .open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false)
                .unwrap();
            let payload = format!("payload-{i}-0123456789").into_bytes();
            // Send in two chunks, no fin yet, so the server must read this
            // incrementally across polls, like a real subgroup header+body.
            let (first, second) = payload.split_at(payload.len() / 2);
            c_wt.stream_send(stream_id, &mut c_h3, &mut pipe.client, first, false).unwrap();
            stream_ids.push((stream_id, second.to_vec()));
        }

        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server); // must not panic

        // Cancel a couple of streams mid-flight via raw QUIC reset.
        for &(stream_id, _) in stream_ids.iter().take(3) {
            pipe.client
                .stream_shutdown(stream_id, quiche::Shutdown::Write, 42)
                .unwrap();
        }

        // Finish the rest normally: send the second half + fin.
        for &(stream_id, ref second) in stream_ids.iter().skip(3) {
            c_wt.stream_send(stream_id, &mut c_h3, &mut pipe.client, second, true).unwrap();
        }

        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server); // must not panic

        // Drain everything: read what's available on each stream, possibly
        // over multiple partial reads, interleaved with more polls (as a
        // real event loop would do while more data/resets keep arriving).
        for _ in 0..5 {
            for &(stream_id, _) in &stream_ids {
                let mut b = [0u8; 4];
                loop {
                    match s_wt.recv_stream(stream_id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(_) => break, // Done, Fin, or the reset's StreamReset
                    }
                }
            }
            pipe.advance().unwrap();
            s_wt.poll_h3(&mut s_h3, &mut pipe.server);
            s_wt.poll(&mut s_h3, &mut pipe.server); // must not panic
        }
    }

    /// A stream's state is forgotten once both directions ended, not kept for the connection's lifetime.
    #[test]
    fn ended_streams_are_collected() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();
        let before = c_wt.stream_count();

        let finished = c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false).unwrap();
        c_wt.stream_send(finished, &mut c_h3, &mut pipe.client, b"x", true).unwrap();
        let reset = c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false).unwrap();
        c_wt.stream_send(reset, &mut c_h3, &mut pipe.client, b"x", false).unwrap();
        c_wt.stream_send_ended(reset);
        assert_eq!(c_wt.stream_count(), before, "uni streams end with their send side");

        let bidi = c_wt.open_stream(wt_session_id, &mut c_h3, &mut pipe.client, true).unwrap();
        c_wt.stream_send(bidi, &mut c_h3, &mut pipe.client, b"x", true).unwrap();
        assert_eq!(c_wt.stream_count(), before + 1, "a bidi stream can still be read");

        pipe.advance().unwrap();
        s_wt.poll_h3(&mut s_h3, &mut pipe.server);
        s_wt.poll(&mut s_h3, &mut pipe.server);
        s_wt.stream_send(bidi, &mut s_h3, &mut pipe.server, b"y", true).unwrap();
        pipe.advance().unwrap();
        c_wt.poll_h3(&mut c_h3, &mut pipe.client);
        c_wt.poll(&mut c_h3, &mut pipe.client);
        let mut b = [0u8; 4];
        while c_wt.recv_stream(bidi, wt_session_id, &mut c_h3, &mut pipe.client, &mut b).is_ok() {}
        assert_eq!(c_wt.stream_count(), before);
    }
}

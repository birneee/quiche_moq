mod connection;
mod error;
mod pending_request;
mod pending_response;
mod session;
mod stream;
pub mod test_utils;

pub use crate::connection::Connection;
pub use crate::error::Error;

type SessionId = u64;

/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-02.html#name-unidirectional-streams
pub const WEBTRANSPORT_UNI_STREAM_TYPE_ID: u64 = 0x54;
/// https://www.ietf.org/archive/id/draft-ietf-webtrans-http3-02.html#name-bidirectional-streams
pub const WEBTRANSPORT_BIDI_STREAM_TYPE_ID: u64 = 0x41;

/// https://www.rfc-editor.org/rfc/rfc9000#name-variable-length-integer-enc
const MAX_VARINT_LEN: usize = 8;

/// RFC8441
const PROTOCOL_HEADER_WEBTRANSPORT: &[u8] = b"webtransport";

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

/// Configure H3 for WebTransport.
pub fn configure_h3(c: &mut quiche::h3::Config) -> Result<(), quiche::h3::Error> {
    c.enable_extended_connect(true);
    c.enable_webtransport_streams(true);
    c.set_additional_settings(vec![
        (ENABLE_WEBTRANSPORT_H3_SETTINGS_PARAMETER_ID, 1),
        (WT_MAX_SESSIONS_H3_SETTINGS_PARAMETER_ID, 1),
    ])?;
    Ok(())
}

/// Check if the H3 peer has enabled WebTransport.
/// Returns `false` if the settings have not been received yet.
pub fn webtransport_enabled_by_server(c: &quiche::h3::Connection) -> bool {
    if !c.extended_connect_enabled_by_peer() {
        return false;
    }

    for &(key, value) in c.peer_settings_raw().into_iter().flatten() {
        match key {
            ENABLE_WEBTRANSPORT_H3_SETTINGS_PARAMETER_ID => {
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
    use crate::test_utils::_init_webtransport_pipe;
    use quiche::h3;

    #[test]
    fn send_on_unidirectional_stream() {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

        const MSG: &[u8] = b"hello";

        let wt_stream_id = c_wt
            .open_stream(wt_session_id, &mut c_h3, &mut pipe.client, false)
            .unwrap();
        c_wt.stream_send(wt_stream_id, &mut pipe.client, MSG, false)
            .unwrap();

        pipe.advance().unwrap();

        matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done));
        s_wt.poll(&mut s_h3, &mut pipe.server);
        assert_eq!(s_wt.readable_streams(wt_session_id), &[wt_stream_id]);
        let mut b = [0u8; 100];
        let len = s_wt.recv_stream(wt_stream_id, wt_session_id, &mut s_h3, &mut pipe.server, &mut b).unwrap();
        assert_eq!(&b[..len], MSG);
    }
}

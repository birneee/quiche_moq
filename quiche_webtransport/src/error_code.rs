//! WebTransport application error codes of data streams, remapped into HTTP/3's error code space
//! (draft-ietf-webtrans-http3-16 §4.4, the same mapping since draft 02).

use crate::Error;

const FIRST: u64 = 0x52e4a40fa8db;
const LAST: u64 = 0x52e5ac983162;

/// The HTTP/3 error code a RESET_STREAM or STOP_SENDING carries for application error `code`.
pub fn to_http3(code: u32) -> u64 {
    let code = u64::from(code);
    FIRST + code + code / 0x1e
}

/// The application error of an HTTP/3 error code, `None` outside the range or on a reserved codepoint.
pub fn from_http3(code: u64) -> Option<u32> {
    if !(FIRST..=LAST).contains(&code) || (code - 0x21).is_multiple_of(0x1f) {
        return None;
    }
    let shifted = code - FIRST;
    u32::try_from(shifted - shifted / 0x1f).ok()
}

/// A data stream's RESET_STREAM or STOP_SENDING with its application error,
/// codes outside the range (peers that don't remap) unchanged.
pub(crate) fn decode_stream_error(e: Error) -> Error {
    let app = |code: u64| from_http3(code).map_or(code, u64::from);
    match e {
        Error::StreamReset(code) => Error::StreamReset(app(code)),
        Error::StreamStopped(code) => Error::StreamStopped(app(code)),
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_bounds() {
        assert_eq!(to_http3(0), FIRST);
        assert_eq!(to_http3(u32::MAX), LAST);
        assert_eq!(from_http3(FIRST), Some(0));
        assert_eq!(from_http3(LAST), Some(u32::MAX));
        assert_eq!(from_http3(FIRST - 1), None);
        assert_eq!(from_http3(LAST + 1), None);
    }

    /// The first reserved codepoint lies between application errors 0x1d and 0x1e.
    #[test]
    fn reserved_codepoints_are_skipped() {
        assert_eq!(to_http3(0x1d) + 2, to_http3(0x1e));
        assert!((to_http3(0x1d) + 1 - 0x21).is_multiple_of(0x1f));
        assert_eq!(from_http3(to_http3(0x1d) + 1), None);
    }

    #[test]
    fn round_trips() {
        for code in (0..0x200).chain([0x1234_5678, u32::MAX - 1]) {
            assert_eq!(from_http3(to_http3(code)), Some(code));
        }
    }
}

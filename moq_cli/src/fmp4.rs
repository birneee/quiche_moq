/// Detect whether a byte buffer begins with an fMP4 box.
///
/// The first 8 bytes of an fMP4 stream are a 4-byte box size followed by a
/// 4-byte box type. Valid leading types are `ftyp`, `moov`, `mdat`, `free`, `skip`.
pub fn is_fmp4(buf: &[u8]) -> bool {
    if buf.len() < 8 {
        return false;
    }
    matches!(&buf[4..8], b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(b"ftyp")]
    #[case(b"moov")]
    #[case(b"mdat")]
    fn fmp4_detected_from_box_type(#[case] box_type: &[u8; 4]) {
        let mut buf = [0u8; 8];
        buf[4..8].copy_from_slice(box_type);
        assert!(is_fmp4(&buf));
    }

    #[rstest]
    #[case(b"hello\nworld\n")]
    #[case(b"ftyp")]
    #[case(b"")]
    fn not_fmp4(#[case] buf: &[u8]) {
        assert!(!is_fmp4(buf));
    }
}

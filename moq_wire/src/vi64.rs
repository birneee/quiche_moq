//! The variable-length integer encoding introduced in
//! draft-ietf-moq-transport-17 §1.4.1, used by [`crate::Version`] `>=`
//! [`crate::MOQ_VERSION_DRAFT_17`] in place of the QUIC-style (RFC 9000
//! §16) 2-bit-length-prefix varint that `octets::Octets::get_varint` reads
//! for every earlier draft.
//!
//! The encoded length is the number of leading 1 bits in the first byte
//! (0 leading ones → 1 byte, ... 7 leading ones → 8 bytes, first byte
//! entirely 1s → 9 bytes). This supports the full 64-bit range in 9 bytes
//! instead of the QUIC varint's 62-bit range in 8.
//!
//! draft-ietf-moq-transport-17 leaves the 7-byte length (6 leading ones)
//! undefined — "11111100 is an invalid code point" — and
//! draft-ietf-moq-transport-18 §1.4.1 assigns it (49 usable bits). Callers
//! pass `allow_7byte` (`version >= MOQ_VERSION_DRAFT_18`) to select which
//! rule applies.

use crate::error::{Error, Result};
use crate::{Version, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
use octets::{Octets, OctetsMut};

/// Reads a varint using whichever encoding `version` specifies: this
/// module's vi64 for `MOQ_VERSION_DRAFT_17`/`_18`, or the QUIC-style
/// varint `octets` provides natively for every other (older, or
/// non-IETF-draft) version. Matched by exact version rather than a `>=`
/// range so unrecognised versions (including
/// `MOQ_VERSION_LITE_01_BY_KIXELATED`, which is numerically larger than
/// the draft constants despite not being a newer draft) fall back to the
/// old varint instead of silently misparsing.
pub fn read_varint(b: &mut Octets, version: Version) -> Result<u64> {
    match version {
        MOQ_VERSION_DRAFT_17 => get_vi64(b, false),
        MOQ_VERSION_DRAFT_18 => get_vi64(b, true),
        _ => Ok(b.get_varint()?),
    }
}

/// Writes a varint using whichever encoding `version` specifies. See
/// [`read_varint`]. Returns the number of bytes written.
pub fn write_varint(b: &mut OctetsMut, v: u64, version: Version) -> Result<usize> {
    match version {
        MOQ_VERSION_DRAFT_17 => put_vi64(b, v, false),
        MOQ_VERSION_DRAFT_18 => put_vi64(b, v, true),
        _ => Ok(b.put_varint(v)?.len()),
    }
}

/// Number of bytes a vi64 starting with `first_byte` will take, without
/// requiring the rest of the buffer to be present. Mirrors
/// `octets::varint_parse_len` for the old QUIC varint.
fn vi64_parse_len(first_byte: u8) -> usize {
    let leading_ones = first_byte.leading_ones() as usize;
    if leading_ones == 8 { 9 } else { leading_ones + 1 }
}

/// Like [`get_vi64`] but does not advance `b`'s cursor.
pub fn peek_vi64(b: &mut Octets, allow_7byte: bool) -> Result<u64> {
    let first = b.peek_u8()?;
    let len = vi64_parse_len(first);
    let mut peeked = b.peek_bytes(len)?;
    get_vi64(&mut peeked, allow_7byte)
}

/// Reads a varint without advancing `b`'s cursor, dispatching on `version`
/// like [`read_varint`].
pub fn peek_varint(b: &mut Octets, version: Version) -> Result<u64> {
    match version {
        MOQ_VERSION_DRAFT_17 => peek_vi64(b, false),
        MOQ_VERSION_DRAFT_18 => peek_vi64(b, true),
        _ => crate::octets::peek_varint(b),
    }
}

/// Reads a vi64. See the module docs for `allow_7byte`.
pub fn get_vi64(b: &mut Octets, allow_7byte: bool) -> Result<u64> {
    let first = b.peek_u8()?;
    let leading_ones = first.leading_ones() as usize; // 0..=8

    if leading_ones == 6 && !allow_7byte {
        return Err(Error::ProtocolViolation(
            "7-byte vi64 prefix (6 leading ones) is not a valid code point before \
             draft-ietf-moq-transport-18"
                .into(),
        ));
    }

    let len = if leading_ones == 8 { 9 } else { leading_ones + 1 };
    let bytes = b.get_bytes(len)?;
    let raw = bytes.as_ref();

    // The first byte's low `8 - (leading_ones + 1)` bits (0 once len == 9,
    // since then the whole byte is the marker) are value bits; the rest of
    // `raw` is a plain big-endian continuation.
    let mut v = (raw[0] as u64) & (0xFFu64 >> (leading_ones + 1).min(8));
    for &byte in &raw[1..] {
        v = (v << 8) | byte as u64;
    }
    Ok(v)
}

/// Writes `v` using the shortest valid vi64 encoding. See the module docs
/// for `allow_7byte`. Returns the number of bytes written.
pub fn put_vi64(b: &mut OctetsMut, v: u64, allow_7byte: bool) -> Result<usize> {
    let len = min_len(v, allow_7byte);
    if len == 9 {
        b.put_u8(0xFF)?;
        b.put_bytes(&v.to_be_bytes())?;
        return Ok(9);
    }

    let leading_ones = (len - 1) as u32; // 0..=7
    // Top `leading_ones` bits set, then a 0 (e.g. leading_ones=1 -> 0x80).
    let marker: u8 = 0xFFu8.checked_shl(8 - leading_ones).unwrap_or(0);

    let value_bytes = v.to_be_bytes();
    let mut out = [0u8; 8];
    out[8 - len..].copy_from_slice(&value_bytes[8 - len..]);
    out[8 - len] |= marker;
    b.put_bytes(&out[8 - len..])?;
    Ok(len)
}

/// The number of bytes `v` would take to encode, without writing anything.
/// See [`read_varint`] for `version` dispatch semantics.
pub fn varint_len(v: u64, version: Version) -> usize {
    match version {
        MOQ_VERSION_DRAFT_17 => min_len(v, false),
        MOQ_VERSION_DRAFT_18 => min_len(v, true),
        _ => octets::varint_len(v),
    }
}

fn min_len(v: u64, allow_7byte: bool) -> usize {
    if v <= 0x7F {
        1
    } else if v <= 0x3FFF {
        2
    } else if v <= 0x1F_FFFF {
        3
    } else if v <= 0xFFF_FFFF {
        4
    } else if v <= 0x7_FFFF_FFFF {
        5
    } else if v <= 0x3FF_FFFF_FFFF {
        6
    } else if allow_7byte && v <= 0x1_FFFF_FFFF_FFFF {
        7
    } else if v <= 0xFF_FFFF_FFFF_FFFF {
        8
    } else {
        9
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn decode(bytes: &[u8], allow_7byte: bool) -> u64 {
        let mut o = Octets::with_slice(bytes);
        get_vi64(&mut o, allow_7byte).unwrap()
    }

    #[rstest]
    #[case(crate::MOQ_VERSION_DRAFT_16)]
    #[case(crate::MOQ_VERSION_LITE_01_BY_KIXELATED)]
    fn dispatch_uses_old_varint_below_draft_17(#[case] version: Version) {
        let mut buf = [0u8; 8];
        let mut w = OctetsMut::with_slice(&mut buf);
        write_varint(&mut w, 300, version).unwrap();
        let mut r = Octets::with_slice(&buf);
        assert_eq!(read_varint(&mut r, version).unwrap(), 300);
        // Old QUIC varint: 300 needs the 2-byte (0b01......) form, not vi64's.
        assert_eq!(buf[0] & 0xC0, 0x40);
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_17)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn dispatch_uses_vi64_from_draft_17(#[case] version: Version) {
        let mut buf = [0u8; 9];
        let mut w = OctetsMut::with_slice(&mut buf);
        let n = write_varint(&mut w, 70_423_237_261_249_041, version).unwrap();
        let mut r = Octets::with_slice(&buf[..n]);
        assert_eq!(read_varint(&mut r, version).unwrap(), 70_423_237_261_249_041);
    }

    #[test]
    fn dispatch_seven_byte_gated_by_draft_18() {
        // A value whose minimal encoding is 7 bytes only under draft-18.
        let v = 0x400_0000_0000u64;
        let mut buf17 = [0u8; 9];
        let n17 = write_varint(&mut OctetsMut::with_slice(&mut buf17), v, MOQ_VERSION_DRAFT_17).unwrap();
        assert_eq!(n17, 8);
        let mut buf18 = [0u8; 9];
        let n18 = write_varint(&mut OctetsMut::with_slice(&mut buf18), v, MOQ_VERSION_DRAFT_18).unwrap();
        assert_eq!(n18, 7);
    }

    fn encode(v: u64, allow_7byte: bool) -> Vec<u8> {
        let mut buf = [0u8; 9];
        let mut o = OctetsMut::with_slice(&mut buf);
        let n = put_vi64(&mut o, v, allow_7byte).unwrap();
        buf[..n].to_vec()
    }

    // Worked examples from draft-ietf-moq-transport-18 §1.4.1 Table 2
    // (draft-17's table shares all but the corrected 4-byte example and
    // lacks the 7-byte one, since that length isn't valid there).
    #[rstest]
    #[case(&[0x25], 37)]
    #[case(&[0x80, 0x25], 37)] // non-minimal 2-byte encoding of the same value
    #[case(&[0xbb, 0xbd], 15_293)]
    #[case(&[0xed, 0x7f, 0x3e, 0x7d], 226_442_877)]
    #[case(&[0xfa, 0xa1, 0xa0, 0xe4, 0x03, 0xd8], 2_893_212_287_960)]
    #[case(&[0xfe, 0xfa, 0x31, 0x8f, 0xa8, 0xe3, 0xca, 0x11], 70_423_237_261_249_041)]
    #[case(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], u64::MAX)]
    fn draft_examples_decode(#[case] bytes: &[u8], #[case] expected: u64) {
        assert_eq!(decode(bytes, true), expected);
    }

    #[test]
    fn draft_18_seven_byte_example_decodes_only_with_allow_7byte() {
        let bytes = [0xfc, 0x89, 0x98, 0xab, 0xc6, 0x6b, 0xc0];
        assert_eq!(decode(&bytes, true), 151_288_809_941_952);

        let mut o = Octets::with_slice(&bytes);
        assert!(matches!(
            get_vi64(&mut o, false),
            Err(Error::ProtocolViolation(_))
        ));
    }

    #[rstest]
    #[case(1, false)]
    #[case(1, true)]
    #[case(0x7F, false)]
    #[case(0x80, false)] // smallest value needing 2 bytes
    #[case(0x3FFF, false)]
    #[case(0x4000, false)] // smallest value needing 3 bytes
    #[case(0x1F_FFFF, false)]
    #[case(0x20_0000, false)] // smallest value needing 4 bytes
    #[case(0xFFF_FFFF, false)]
    #[case(0x1000_0000, false)] // smallest value needing 5 bytes
    #[case(0x7_FFFF_FFFF, false)]
    #[case(0x8_0000_0000, false)] // smallest value needing 6 bytes
    #[case(0x3FF_FFFF_FFFF, false)]
    #[case(0x400_0000_0000, false)] // needs 7 bytes if allowed, else 8
    #[case(0x400_0000_0000, true)]
    #[case(0x1_FFFF_FFFF_FFFF, true)] // largest 7-byte value
    #[case(0x2_0000_0000_0000, true)] // smallest value needing 8 bytes
    #[case(0xFF_FFFF_FFFF_FFFF, false)]
    #[case(0x100_0000_0000_0000, false)] // smallest value needing 9 bytes
    #[case(u64::MAX, false)]
    #[case(u64::MAX, true)]
    fn roundtrip(#[case] v: u64, #[case] allow_7byte: bool) {
        let bytes = encode(v, allow_7byte);
        assert_eq!(decode(&bytes, true), v);
    }

    #[test]
    fn encodes_minimally() {
        assert_eq!(encode(37, false), vec![0x25]);
        assert_eq!(encode(0x400_0000_0000, false).len(), 8);
        assert_eq!(encode(0x400_0000_0000, true).len(), 7);
    }

    #[test]
    fn seven_byte_prefix_rejected_before_draft_18() {
        let bytes = [0xfc, 0, 0, 0, 0, 0, 0];
        let mut o = Octets::with_slice(&bytes);
        assert!(matches!(
            get_vi64(&mut o, false),
            Err(Error::ProtocolViolation(_))
        ));
    }

    #[test]
    fn short_buffer_is_an_error() {
        let bytes = [0xff]; // claims 9 bytes total, only 1 present
        let mut o = Octets::with_slice(&bytes);
        assert!(get_vi64(&mut o, true).is_err());
    }
}

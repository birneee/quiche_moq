use crate::{DecodeError, EncodeError};

/// How a varint is laid out, fixed by the negotiated MoQ version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarIntForm {
    /// RFC 9000 §16: a 2-bit length tag, up to 2^62 - 1; MoQ until draft 16.
    Quic,
    /// draft-ietf-moq-transport-17 §1.4.1: leading 1 bits count the extra bytes, up to `u64::MAX`.
    /// - `seven_bytes`: whether 6 leading 1 bits are a valid 7-byte length, draft 18 on
    LeadingOnes { seven_bytes: bool },
}

/// The longest varint of any form.
pub const MAX_LEN: usize = 9;

impl VarIntForm {
    pub const QUIC_MAX: u64 = (1 << 62) - 1;

    /// Bytes of the shortest encoding of `v`.
    pub const fn encoded_len(self, v: u64) -> Result<usize, EncodeError> {
        Ok(match self {
            Self::Quic => match v {
                0..0x40 => 1,
                0x40..0x4000 => 2,
                0x4000..0x4000_0000 => 4,
                0x4000_0000..=Self::QUIC_MAX => 8,
                _ => return Err(EncodeError("QUIC varint of 2^62 or more")),
            },
            Self::LeadingOnes { seven_bytes } => match v {
                0..0x80 => 1,
                0x80..0x4000 => 2,
                0x4000..0x20_0000 => 3,
                0x20_0000..0x1000_0000 => 4,
                0x1000_0000..0x8_0000_0000 => 5,
                0x8_0000_0000..0x400_0000_0000 => 6,
                0x400_0000_0000..0x2_0000_0000_0000 if seven_bytes => 7,
                0x400_0000_0000..0x100_0000_0000_0000 => 8,
                _ => 9,
            },
        })
    }

    /// Bytes of the varint starting with `first`.
    pub(crate) const fn read_len(self, first: u8) -> Result<usize, DecodeError> {
        match self {
            Self::Quic => Ok(1 << (first >> 6)),
            Self::LeadingOnes { seven_bytes } => match first.leading_ones() {
                6 if !seven_bytes => Err(DecodeError::Malformed("7-byte varint before draft 18")),
                8 => Ok(9),
                ones => Ok(ones as usize + 1),
            },
        }
    }

    /// The varint at the start of `buf` and its length, `None` if `buf` ends first.
    pub(crate) fn read(self, buf: &[u8]) -> Result<Option<(u64, usize)>, DecodeError> {
        let Some(&first) = buf.first() else { return Ok(None) };
        let len = self.read_len(first)?;
        let Some(bytes) = buf.get(..len) else { return Ok(None) };
        let mut be = [0u8; 8];
        let value = match (self, len) {
            (_, 9) => {
                be.copy_from_slice(&bytes[1..]);
                u64::from_be_bytes(be)
            }
            (Self::Quic, _) => {
                be[8 - len..].copy_from_slice(bytes);
                be[8 - len] &= 0x3F;
                u64::from_be_bytes(be)
            }
            (Self::LeadingOnes { .. }, _) => {
                be[8 - len..].copy_from_slice(bytes);
                be[8 - len] &= 0xFFu8.checked_shr(len as u32).unwrap_or(0);
                u64::from_be_bytes(be)
            }
        };
        Ok(Some((value, len)))
    }

    /// The shortest encoding of `v`, in the first `len` bytes of the result.
    pub(crate) fn encode(self, v: u64) -> Result<([u8; MAX_LEN], usize), EncodeError> {
        let len = self.encoded_len(v)?;
        let mut out = [0u8; MAX_LEN];
        if len == 9 {
            out[0] = 0xFF;
            out[1..].copy_from_slice(&v.to_be_bytes());
            return Ok((out, len));
        }
        out[..len].copy_from_slice(&(v | self.length_marker(len)).to_be_bytes()[8 - len..]);
        Ok((out, len))
    }

    /// The marker in the top bits of a `len`-byte varint, `len` < 9.
    const fn length_marker(self, len: usize) -> u64 {
        let bits = len as u32 * 8;
        match self {
            Self::Quic => (len.trailing_zeros() as u64) << (bits - 2),
            // `len - 1` ones, then a zero
            Self::LeadingOnes { .. } => ((1 << (len - 1)) - 1) << (bits - len as u32 + 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const QUIC: VarIntForm = VarIntForm::Quic;
    const LO17: VarIntForm = VarIntForm::LeadingOnes { seven_bytes: false };
    const LO18: VarIntForm = VarIntForm::LeadingOnes { seven_bytes: true };

    fn encoded(form: VarIntForm, v: u64) -> Vec<u8> {
        let (out, len) = form.encode(v).unwrap();
        out[..len].to_vec()
    }

    /// RFC 9000 Appendix A.1.
    #[test]
    fn quic_examples() {
        for (bytes, v) in [
            (&[0xc2, 0x19, 0x7c, 0x5e, 0xff, 0x14, 0xe8, 0x8c][..], 151_288_809_941_952_652),
            (&[0x9d, 0x7f, 0x3e, 0x7d], 494_878_333),
            (&[0x7b, 0xbd], 15_293),
            (&[0x25], 37),
        ] {
            assert_eq!(QUIC.read(bytes), Ok(Some((v, bytes.len()))));
            assert_eq!(encoded(QUIC, v), bytes);
        }
        assert_eq!(QUIC.read(&[0x40, 0x25]), Ok(Some((37, 2))), "non-minimal");
    }

    /// draft-ietf-moq-transport-18 §1.4.1 Table 2.
    #[test]
    fn leading_ones_examples() {
        for (bytes, v) in [
            (&[0x25][..], 37),
            (&[0xbb, 0xbd], 15_293),
            (&[0xed, 0x7f, 0x3e, 0x7d], 226_442_877),
            (&[0xfa, 0xa1, 0xa0, 0xe4, 0x03, 0xd8], 2_893_212_287_960),
            (&[0xfc, 0x89, 0x98, 0xab, 0xc6, 0x6b, 0xc0], 151_288_809_941_952),
            (&[0xfe, 0xfa, 0x31, 0x8f, 0xa8, 0xe3, 0xca, 0x11], 70_423_237_261_249_041),
            (&[0xff; 9], u64::MAX),
        ] {
            assert_eq!(LO18.read(bytes), Ok(Some((v, bytes.len()))));
            assert_eq!(encoded(LO18, v), bytes);
        }
        assert_eq!(LO18.read(&[0x80, 0x25]), Ok(Some((37, 2))), "non-minimal");
        assert!(matches!(LO17.read(&[0xfc, 0, 0, 0, 0, 0, 0]), Err(DecodeError::Malformed(_))));
        assert_eq!(encoded(LO17, 0x400_0000_0000).len(), 8);
    }

    #[test]
    fn every_length_boundary_round_trips() {
        for form in [QUIC, LO17, LO18] {
            let max = if form == QUIC { VarIntForm::QUIC_MAX } else { u64::MAX };
            for shift in 0..64 {
                for v in [(1u64 << shift) - 1, 1 << shift, (1u64 << shift) + 1] {
                    if v > max {
                        continue;
                    }
                    let bytes = encoded(form, v);
                    assert_eq!(bytes.len(), form.encoded_len(v).unwrap());
                    assert_eq!(form.read(&bytes), Ok(Some((v, bytes.len()))), "{form:?} {v:#x}");
                    assert_eq!(form.read(&bytes[..bytes.len() - 1]), Ok(None));
                }
            }
        }
        assert!(QUIC.encode(1 << 62).is_err());
    }
}

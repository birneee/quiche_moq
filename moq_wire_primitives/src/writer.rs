use alloc::vec::Vec;

use crate::{EncodeError, VarIntForm};

/// Appends to a `Vec`, reuse one to encode without allocating.
pub struct Writer<'a> {
    buf: &'a mut Vec<u8>,
    form: VarIntForm,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut Vec<u8>, form: VarIntForm) -> Self {
        Self { buf, form }
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.bytes(&v.to_be_bytes());
    }

    /// A varint of this writer's form.
    pub fn varint(&mut self, v: u64) -> Result<(), EncodeError> {
        let (encoded, len) = self.form.encode(v)?;
        self.bytes(&encoded[..len]);
        Ok(())
    }

    /// A varint length, then `v`.
    pub fn length_prefixed_bytes(&mut self, v: &[u8]) -> Result<(), EncodeError> {
        self.varint(v.len() as u64)?;
        self.bytes(v);
        Ok(())
    }

    /// What `body` writes, after its length as a `u16`.
    /// Nothing is left behind if it fails.
    pub fn with_u16_length<E: From<EncodeError>>(&mut self, body: impl FnOnce(&mut Self) -> Result<(), E>) -> Result<(), E> {
        let start = self.buf.len();
        self.u16(0);
        self.undo_on_error(start, body)?;
        let Ok(len) = u16::try_from(self.buf.len() - start - 2) else {
            self.buf.truncate(start);
            return Err(EncodeError("body longer than a u16 length").into());
        };
        self.buf[start..start + 2].copy_from_slice(&len.to_be_bytes());
        Ok(())
    }

    /// What `body` writes, after its length as the shortest varint.
    /// Nothing is left behind if it fails.
    pub fn with_varint_length<E: From<EncodeError>>(&mut self, body: impl FnOnce(&mut Self) -> Result<(), E>) -> Result<(), E> {
        let start = self.buf.len();
        self.all_or_nothing(body)?;
        let (prefix, len) = self.form.encode((self.buf.len() - start) as u64)?;
        self.buf.splice(start..start, prefix[..len].iter().copied());
        Ok(())
    }

    /// What `body` writes, nothing if it fails.
    pub fn all_or_nothing<E>(&mut self, body: impl FnOnce(&mut Self) -> Result<(), E>) -> Result<(), E> {
        let start = self.buf.len();
        self.undo_on_error(start, body)
    }

    fn undo_on_error<E>(&mut self, start: usize, body: impl FnOnce(&mut Self) -> Result<(), E>) -> Result<(), E> {
        body(self).inspect_err(|_| self.buf.truncate(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DecodeError, Reader};
    use alloc::vec;

    #[test]
    fn length_prefixes_are_filled_in() {
        let mut buf = vec![];
        let mut w = Writer::new(&mut buf, VarIntForm::Quic);
        w.with_u16_length(|w| w.length_prefixed_bytes(&[7; 3])).unwrap();
        w.with_varint_length(|w| {
            w.bytes(&[1; 100]);
            Ok::<_, EncodeError>(())
        })
        .unwrap();
        assert_eq!(buf[..6], [0, 4, 3, 7, 7, 7]);
        let mut r = Reader::new(&buf[6..], VarIntForm::Quic);
        assert_eq!(r.length_prefixed_bytes(), Ok(&[1; 100][..]));
        assert!(r.is_empty());
    }

    #[test]
    fn failed_body_leaves_nothing() {
        let mut buf = vec![9];
        let mut w = Writer::new(&mut buf, VarIntForm::Quic);
        let failed = w.with_u16_length(|w| {
            w.u8(1);
            w.varint(u64::MAX)
        });
        assert!(failed.is_err());
        assert!(w.with_u16_length(|w| { w.bytes(&[0; 1 << 16]); Ok::<_, EncodeError>(()) }).is_err());
        assert_eq!(buf, [9]);
    }

    #[test]
    fn round_trips_through_reader() {
        let mut buf = vec![];
        let mut w = Writer::new(&mut buf, VarIntForm::LeadingOnes { seven_bytes: true });
        w.u8(1);
        w.u16(2);
        w.varint(u64::MAX).unwrap();
        let mut r = Reader::new(&buf, VarIntForm::LeadingOnes { seven_bytes: true });
        assert_eq!((r.u8(), r.u16(), r.varint()), (Ok(1), Ok(2), Ok(u64::MAX)));
        assert_eq!(r.u8(), Err(DecodeError::Incomplete { needed: buf.len() + 1 }));
    }
}

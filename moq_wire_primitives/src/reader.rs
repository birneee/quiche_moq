use core::marker::PhantomData;

use crate::{DecodeError, VarIntForm};

/// What running short means to a [`Reader`].
pub trait Mode: sealed::Sealed {
    #[doc(hidden)]
    fn short(needed: usize) -> DecodeError;
}

/// Input that may continue: running short is [`DecodeError::Incomplete`].
#[derive(Debug, Clone, Copy)]
pub struct Stream;

/// A complete frame: running short or leaving bytes is [`DecodeError::Malformed`].
#[derive(Debug, Clone, Copy)]
pub struct Frame;

impl Mode for Stream {
    fn short(needed: usize) -> DecodeError {
        DecodeError::Incomplete { needed }
    }
}

impl Mode for Frame {
    fn short(_: usize) -> DecodeError {
        DecodeError::Malformed("runs past the end of its frame")
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Stream {}
    impl Sealed for super::Frame {}
}

/// Decodes from the front of borrowed input.
/// - a failed read consumes nothing
/// - clone it to peek
#[derive(Debug)]
pub struct Reader<'a, M: Mode = Stream> {
    buf: &'a [u8],
    start_len: usize,
    form: VarIntForm,
    mode: PhantomData<M>,
}

impl<M: Mode> Clone for Reader<'_, M> {
    fn clone(&self) -> Self {
        Self { buf: self.buf, start_len: self.start_len, form: self.form, mode: PhantomData }
    }
}

impl<'a> Reader<'a, Stream> {
    pub fn new(buf: &'a [u8], form: VarIntForm) -> Self {
        Self { buf, start_len: buf.len(), form, mode: PhantomData }
    }
}

impl<'a> Reader<'a, Frame> {
    /// Reads `buf` as one complete frame, e.g. a datagram.
    pub fn frame_of(buf: &'a [u8], form: VarIntForm) -> Self {
        Self { buf, start_len: buf.len(), form, mode: PhantomData }
    }

    /// Ends the frame, bytes left over are malformed.
    pub fn finish(self) -> Result<(), DecodeError> {
        match self.buf.is_empty() {
            true => Ok(()),
            false => Err(DecodeError::Malformed("bytes left after the end of the frame")),
        }
    }
}

impl<'a, M: Mode> Reader<'a, M> {
    /// Bytes read since the start.
    pub fn consumed(&self) -> usize {
        self.start_len - self.buf.len()
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn short(&self, more: usize) -> DecodeError {
        M::short(self.consumed().saturating_add(more))
    }

    /// The next `len` bytes; a length beyond the address space is malformed, even on 32-bit targets.
    pub fn bytes(&mut self, len: u64) -> Result<&'a [u8], DecodeError> {
        let len = usize::try_from(len).map_err(|_| DecodeError::Malformed("length beyond the address space"))?;
        let (head, rest) = self.buf.split_at_checked(len).ok_or_else(|| self.short(len))?;
        self.buf = rest;
        Ok(head)
    }

    /// Everything left.
    pub fn rest(&mut self) -> &'a [u8] {
        core::mem::take(&mut self.buf)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, rest) = self.buf.split_first_chunk::<N>().ok_or_else(|| self.short(N))?;
        self.buf = rest;
        Ok(*head)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        self.array::<1>().map(|[b]| b)
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        self.array().map(u16::from_be_bytes)
    }

    /// A varint of this reader's form.
    pub fn varint(&mut self) -> Result<u64, DecodeError> {
        let form = self.form;
        match form.read(self.buf)? {
            Some((value, len)) => {
                self.buf = &self.buf[len..];
                Ok(value)
            }
            None => {
                let len = self.buf.first().map_or(Ok(1), |&first| form.read_len(first))?;
                Err(self.short(len))
            }
        }
    }

    /// A varint length, then that many bytes.
    pub fn length_prefixed_bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let mut r = self.clone();
        let len = r.varint()?;
        let bytes = r.bytes(len)?;
        *self = r;
        Ok(bytes)
    }

    /// The next `len` bytes as a complete frame, e.g. a message body.
    pub fn frame(&mut self, len: u64) -> Result<Reader<'a, Frame>, DecodeError> {
        let form = self.form;
        self.bytes(len).map(|buf| Reader::frame_of(buf, form))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIC: VarIntForm = VarIntForm::Quic;

    #[test]
    fn short_read_consumes_nothing_and_says_what_it_needs() {
        let mut r = Reader::new(&[0x05, 0x40], QUIC);
        assert_eq!(r.u8(), Ok(5));
        assert_eq!(r.varint(), Err(DecodeError::Incomplete { needed: 3 }));
        assert_eq!(r.u16(), Err(DecodeError::Incomplete { needed: 3 }));
        assert_eq!(r.remaining(), 1);
    }

    #[test]
    fn frame_running_short_or_left_over_is_malformed() {
        let mut r = Reader::new(&[0x02, 0xAA, 0xBB, 0xCC], QUIC);
        let len = r.varint().unwrap();
        let mut frame = r.frame(len).unwrap();
        assert_eq!(frame.u8(), Ok(0xAA));
        assert!(matches!(frame.u16(), Err(DecodeError::Malformed(_))));
        assert!(matches!(frame.finish(), Err(DecodeError::Malformed(_))));
        assert_eq!(r.rest(), [0xCC]);
    }

    #[test]
    fn length_prefixed_bytes_are_all_or_nothing() {
        let mut r = Reader::new(&[0x03, 1, 2], QUIC);
        assert_eq!(r.length_prefixed_bytes(), Err(DecodeError::Incomplete { needed: 4 }));
        assert_eq!(r.remaining(), 3);
        assert_eq!(Reader::new(&[0x02, 1, 2], QUIC).length_prefixed_bytes(), Ok(&[1, 2][..]));
    }

    /// 2^32 + 1 must not wrap to 1 where `usize` is 32 bits.
    #[test]
    fn huge_length_is_never_truncated() {
        let mut r = Reader::new(&[0xc0, 0, 0, 0x01, 0, 0, 0, 0x01, 0xAA], QUIC);
        let len = r.varint().unwrap();
        assert_eq!(len, (1 << 32) + 1);
        assert!(r.bytes(len).is_err());
        assert_eq!(r.remaining(), 1);
    }
}

use std::cmp::min;
use std::mem;

/// A short fast buffer queue without dynamic allocations.
/// In contrast to a BufReader the buffer is managed manually.
/// This buffer is intended to be used on a data stream between a reader and a writer.
/// When generally the data should not be buffered, but from time a small amount has to be buffered,
/// e.g., for lookahead or parsing.
/// When the buffer length is `0` no extra copy is introduced (`chain_read`).
pub struct ShortBuf<const N: usize> {
    buf: [u8; N],
    offset: usize,
    end: usize,
}

impl<const N: usize> ShortBuf<N> {
    pub fn new() -> Self {
        Self {
            buf: unsafe { mem::MaybeUninit::uninit().assume_init() },
            offset: 0,
            end: 0,
        }
    }

    /// Returns a reference to the buffered data.
    pub fn buffer(&self) -> &[u8] {
        &self.buf[self.offset..self.end]
    }

    pub fn len(&self) -> usize {
        self.end - self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove a given amount of bytes from the front of the buffer.
    pub fn consume(&mut self, amt: usize) {
        assert!(amt <= self.len());
        self.offset += amt;
    }

    /// Remove all bytes from the buffer
    pub fn consume_all(&mut self) {
        self.offset = self.end;
    }

    /// Fill the internal buffer.
    /// `func` can write to the extended buffer directly and must return the amount of bytes added.
    /// If `func` returns an error the buffer will not be extended.
    /// IMPORTANT: consume the full buffer before filling to avoid expensive copies.
    pub fn fill<E, F: FnOnce(&mut [u8]) -> Result<usize, E>>(&mut self, func: F) -> Result<(), E> {
        if self.offset != 0 {
            if self.offset == self.end {
                self.offset = 0;
                self.end = 0;
            } else {
                let len = self.len();
                self.buf.copy_within(self.offset..self.end, 0);
                self.offset = 0;
                self.end = len;
            }
        }
        let len = func(&mut self.buf[self.end..])?;
        self.end += len;
        Ok(())
    }

    /// Similar to `fill` but only fill `n` bytes, instead of the whole buffer.
    pub fn fill_n<E, F: FnOnce(&mut [u8]) -> Result<usize, E>>(
        &mut self,
        func: F,
        n: usize,
    ) -> Result<(), E> {
        self.fill(|b| func(&mut b[..n]))
    }

    /// Similar to `fill` but only fill the buffer to a length of `n` bytes.
    pub fn fill_until<E, F: FnOnce(&mut [u8]) -> Result<usize, E>>(
        &mut self,
        func: F,
        n: usize,
    ) -> Result<(), E> {
        let remaining = n.saturating_sub(self.len());
        if remaining == 0 {
            return Ok(());
        }
        self.fill_n(func, remaining)
    }

    /// Copy the bytes from the buffer to the `dst`.
    /// If the buffer is empty copy bytes directly from `func`.
    /// Read bytes are removed from the buffer.
    /// If `func` returns an error no buffer bytes are removed, however some bytes might be already copied to `dst`.
    #[deprecated]
    pub fn chain_read<E, F: FnOnce(&mut [u8]) -> Result<usize, E>>(
        &mut self,
        func: F,
        dst: &mut [u8],
    ) -> Result<usize, E> {
        match self.is_empty() {
            true => func(dst),
            false => match dst.len() <= self.len() {
                true => {
                    let len = dst.len();
                    dst.copy_from_slice(&self.buf[self.offset..self.offset + len]);
                    self.offset += len;
                    Ok(len)
                }
                false => {
                    let extra_len = self.len();
                    dst[..extra_len].copy_from_slice(self.buffer());
                    let len = func(&mut dst[extra_len..])?;
                    self.offset += extra_len;
                    Ok(len + extra_len)
                }
            },
        }
    }

    pub fn chain_read2<E, F: FnOnce(&mut [u8]) -> Result<usize, E>>(
        &mut self,
        func: F,
        dst: &mut [u8],
    ) -> Result<usize, E> {
        match self.is_empty() {
            true => func(dst),
            false => {
                let len = min(dst.len(), self.len());
                dst[..len].copy_from_slice(&self.buf[self.offset..self.offset + len]);
                self.offset += len;
                Ok(len)
            }
        }
    }

    pub fn read(&mut self, dst: &mut [u8]) -> usize {
        let len = min(self.len(), dst.len());
        dst[..len].copy_from_slice(&self.buf[self.offset..self.offset + len]);
        self.offset += len;
        len
    }

    pub fn remaining_capacity(&self) -> usize {
        N - self.end
    }
}

#[cfg(test)]
mod test {
    use crate::ShortBuf;
    use std::io::{Cursor, Read};

    #[test]
    fn zero_copy() {
        let data: [u8; 3] = [1, 2, 3];
        let mut inner = Cursor::new(data);
        let mut sb = ShortBuf::<0>::new();
        let mut read = [0u8; 5];
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(read[..len], data);
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn read_buffer() {
        let data: [u8; 3] = [1, 2, 3];
        let mut inner = Cursor::new(data);
        let mut sb = ShortBuf::<3>::new();
        sb.fill(|b| inner.read(b)).unwrap();
        assert_eq!(sb.buffer(), data);
        let mut read = [0u8; 5];
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(read[..len], data);
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn read_beyond_buffer() {
        let data: [u8; 3] = [1, 2, 3];
        let mut inner = Cursor::new(data);
        let mut sb = ShortBuf::<2>::new();
        sb.fill(|b| inner.read(b)).unwrap();
        assert_eq!(sb.buffer(), &data[..2]);
        let mut read = [0u8; 5];
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(read[..len], data);
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(len, 0);
    }

    #[test]
    fn overfill() {
        let data: [u8; 3] = [1, 2, 3];
        let mut inner = Cursor::new(data);
        let mut sb = ShortBuf::<3>::new();
        sb.fill(|b| inner.read(b)).unwrap();
        assert_eq!(sb.buffer(), &data);
        sb.fill(|b| inner.read(b)).unwrap();
        assert_eq!(sb.buffer(), &data);
    }

    #[test]
    fn consume() {
        let data: [u8; 3] = [1, 2, 3];
        let mut inner = Cursor::new(data);
        let mut sb = ShortBuf::<3>::new();
        sb.fill(|b| inner.read(b)).unwrap();
        assert_eq!(sb.buffer(), &data);
        sb.consume(3);
        let mut read = [0u8; 5];
        let len = sb.chain_read(|b| inner.read(b), &mut read).unwrap();
        assert_eq!(len, 0);
    }
}

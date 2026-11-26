use octets::{Octets, OctetsMut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capsule{
    ty: u64,
    value: Vec<u8>,
}

impl Capsule{
    
    pub fn new(ty: u64, value: Vec<u8>) -> Self{
        Self {
            ty,
            value,
        }
    }

    pub fn encode(self, buf: &mut [u8]) -> octets::Result<usize> {
        let mut o = OctetsMut::with_slice(buf);
        o.put_varint(self.ty)?;
        o.put_varint(self.value.len() as u64)?;
        o.put_bytes(self.value.as_slice())?;
        Ok(o.off())
    }

    pub fn decode(buf: &[u8]) -> octets::Result<(Capsule, usize)>{
        let mut o = Octets::with_slice(buf);
        let ty = o.get_varint()?;
        let length = o.get_varint()? as usize;
        let value = o.get_bytes(length)?.buf().to_vec();
        Ok((Capsule{ty, value}, o.off()))
    }

}

impl Capsule {
    pub fn ty(&self) -> u64 {
        self.ty
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

/// A kept capsule announced a value longer than the decoder accepts.
#[derive(Debug, PartialEq, Eq)]
pub struct TooLong {
    pub ty: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Kept(Capsule),
    /// a capsule of a type not kept, its value is dropped
    Skipped(u64),
}

/// Decodes a capsule sequence fed in arbitrary pieces (RFC 9297 §3.2).
/// - values of types not kept are skipped, never buffered
/// - buffers at most one kept value of `max_value_len` bytes plus one fed piece
pub struct Decoder {
    buf: Vec<u8>,
    /// bytes of a skipped value still to come
    skip: u64,
    max_value_len: usize,
}

impl Decoder {
    pub fn new(max_value_len: usize) -> Self {
        Self { buf: Vec::new(), skip: 0, max_value_len }
    }

    /// Appends the next piece of the sequence, decode it with [`Self::next`].
    pub fn feed(&mut self, data: &[u8]) {
        let skipped = data.len().min(usize::try_from(self.skip).unwrap_or(usize::MAX));
        self.skip -= skipped as u64;
        self.buf.extend_from_slice(&data[skipped..]);
    }

    /// The next capsule whose header arrived, a kept one only once its value arrived too.
    pub fn next(&mut self, keep: impl Fn(u64) -> bool) -> Result<Option<Decoded>, TooLong> {
        let mut o = Octets::with_slice(&self.buf);
        let (Ok(ty), Ok(len)) = (o.get_varint(), o.get_varint()) else { return Ok(None) };
        let header_len = o.off();
        let available = (self.buf.len() - header_len) as u64;
        if !keep(ty) {
            let dropped = len.min(available);
            self.buf.drain(..header_len + dropped as usize);
            self.skip = len - dropped;
            return Ok(Some(Decoded::Skipped(ty)));
        }
        if len > self.max_value_len as u64 {
            return Err(TooLong { ty, len });
        }
        if available < len {
            return Ok(None);
        }
        let end = header_len + len as usize;
        let value = self.buf[header_len..end].to_vec();
        self.buf.drain(..end);
        Ok(Some(Decoded::Kept(Capsule { ty, value })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEPT: u64 = 0x2843;

    fn encoded(capsules: &[(u64, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for &(ty, value) in capsules {
            let mut buf = vec![0u8; value.len() + 16];
            let len = Capsule::new(ty, value.to_vec()).encode(&mut buf).unwrap();
            out.extend_from_slice(&buf[..len]);
        }
        out
    }

    fn decode_in_pieces(bytes: &[u8], piece: usize, max_value_len: usize) -> Result<Vec<Decoded>, TooLong> {
        let mut decoder = Decoder::new(max_value_len);
        let mut decoded = Vec::new();
        for chunk in bytes.chunks(piece) {
            decoder.feed(chunk);
            while let Some(d) = decoder.next(|ty| ty == KEPT)? {
                decoded.push(d);
            }
        }
        Ok(decoded)
    }

    #[test]
    fn decodes_whatever_the_pieces() {
        let bytes = encoded(&[(0x00, &[7; 300]), (KEPT, b"bye"), (0x78ae, &[]), (KEPT, b"")]);
        let expected = vec![
            Decoded::Skipped(0x00),
            Decoded::Kept(Capsule::new(KEPT, b"bye".to_vec())),
            Decoded::Skipped(0x78ae),
            Decoded::Kept(Capsule::new(KEPT, vec![])),
        ];
        for piece in 1..=bytes.len() {
            assert_eq!(decode_in_pieces(&bytes, piece, 8), Ok(expected.clone()), "piece {piece}");
        }
    }

    #[test]
    fn skipped_values_are_not_buffered() {
        let mut decoder = Decoder::new(8);
        let bytes = encoded(&[(0x00, &[7; 10_000])]);
        decoder.feed(&bytes[..100]);
        assert_eq!(decoder.next(|ty| ty == KEPT), Ok(Some(Decoded::Skipped(0x00))));
        assert!(decoder.buf.is_empty());
        decoder.feed(&bytes[100..]);
        assert_eq!(decoder.next(|ty| ty == KEPT), Ok(None));
        assert_eq!(decoder.skip, 0);
    }

    #[test]
    fn rejects_kept_capsule_over_the_limit() {
        let bytes = encoded(&[(KEPT, &[1; 9])]);
        assert_eq!(decode_in_pieces(&bytes, 3, 8), Err(TooLong { ty: KEPT, len: 9 }));
    }
}

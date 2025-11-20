use octets::{Octets, OctetsMut};

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

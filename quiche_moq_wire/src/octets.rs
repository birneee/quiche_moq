use octets::{varint_parse_len, Octets, OctetsMut};

pub(crate) fn peek_varint(b: &mut Octets) -> crate::error::Result<u64> {
    let first_byte = b.peek_u8()?;
    let varint_len = varint_parse_len(first_byte);
    Ok(b.peek_bytes(varint_len)?.get_varint()?)
}

pub(crate) fn put_u16_at(b: &mut OctetsMut, v: u16, off: usize)-> octets::Result<()> {
    let (_, mut b) = b.split_at(off)?;
    b.put_u16(v)?;
    Ok(())
}

pub(crate) fn put_varint_with_len_at(b: &mut OctetsMut, v: u64, len: usize, off: usize) -> octets::Result<()> {
    let (_, mut b) = b.split_at(off)?;
    b.put_varint_with_len(v, len)?;
    Ok(())
}

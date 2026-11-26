use moq_wire_primitives::{Mode, Reader, Writer};
use crate::error::Result;
use crate::Version;

/// Decodes a value as `version` encodes it.
pub trait FromBytes: Sized {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self>;
}

/// Encodes a value as `version` does.
pub trait ToBytes {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> Result<()>;
}

/// `value` alone, e.g. the bytes of a parameter.
pub(crate) fn to_vec<T: ToBytes>(value: &T, version: Version) -> Result<alloc::vec::Vec<u8>> {
    let mut buf = alloc::vec::Vec::new();
    value.to_bytes(&mut version.writer(&mut buf), version)?;
    Ok(buf)
}

/// The `T` filling `bytes`, e.g. the bytes of a parameter.
pub(crate) fn from_slice<T: FromBytes>(bytes: &[u8], version: Version) -> Result<T> {
    let mut r = version.frame_reader(bytes);
    let value = T::from_bytes(&mut r, version)?;
    r.finish()?;
    Ok(value)
}

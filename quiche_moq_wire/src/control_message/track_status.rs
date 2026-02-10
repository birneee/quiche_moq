use octets::{Octets, OctetsMut};
use crate::{FromBytes, ToBytes, Version, TRACK_STATUS_MESSAGE_ID};
use crate::control_message::ControlMessage;
use crate::namespace::Namespace;

#[allow(unused)]
#[derive(Debug)]
pub struct TrackStatusMessage {
    track_namespace: Namespace,
    track_name: Vec<u8>,
    status_code: u64,
    last_group_id: u64,
    last_object_id: u64,
}

impl ControlMessage for TrackStatusMessage {
    const MESSAGE_IDS: &'static [u64] = &[TRACK_STATUS_MESSAGE_ID];

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        self.track_namespace.to_bytes(b, version)?;
        b.put_varint(self.track_name.len() as u64)?;
        b.put_bytes(&self.track_name)?;
        b.put_varint(self.status_code)?;
        b.put_varint(self.last_group_id)?;
        b.put_varint(self.last_object_id)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let track_namespace = Namespace::from_bytes(b, version)?;
        let track_name_length = b.get_varint()?;
        let track_name = b.get_bytes(track_name_length as usize)?.to_vec();
        let status_code = b.get_varint()?;
        let last_group_id = b.get_varint()?;
        let last_object_id = b.get_varint()?;
        Ok(Self{
            track_namespace,
            track_name,
            status_code,
            last_group_id,
            last_object_id,
        })
    }
}

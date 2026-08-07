use octets::{Octets, OctetsMut};
use crate::vi64;
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

    fn qlog_type_name(&self) -> &'static str { "track_status" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        self.track_namespace.to_bytes(b, version)?;
        vi64::write_varint(b, self.track_name.len() as u64, version)?;
        b.put_bytes(&self.track_name)?;
        vi64::write_varint(b, self.status_code, version)?;
        vi64::write_varint(b, self.last_group_id, version)?;
        vi64::write_varint(b, self.last_object_id, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let track_namespace = Namespace::from_bytes(b, version)?;
        let track_name_length = vi64::read_varint(b, version)?;
        let track_name = b.get_bytes(track_name_length as usize)?.to_vec();
        let status_code = vi64::read_varint(b, version)?;
        let last_group_id = vi64::read_varint(b, version)?;
        let last_object_id = vi64::read_varint(b, version)?;
        Ok(Self{
            track_namespace,
            track_name,
            status_code,
            last_group_id,
            last_object_id,
        })
    }
}

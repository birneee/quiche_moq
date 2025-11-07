use octets::{Octets, OctetsMut};
use crate::{FromBytes, ToBytes, Version, TRACK_STATUS_CONTROL_MESSAGE_ID};
use crate::control_message::header::ControlMessageHeader;
use crate::namespace::Namespace;

#[derive(Debug)]
pub struct TrackStatusMessage {
    track_namespace: Namespace,
    track_name: Vec<u8>,
    status_code: u64,
    last_group_id: u64,
    last_object_id: u64,
}

impl FromBytes for TrackStatusMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), TRACK_STATUS_CONTROL_MESSAGE_ID);
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

impl ToBytes for TrackStatusMessage {
    fn to_bytes(&self, _b: &mut OctetsMut, _version: Version) -> crate::Result<()> {
        todo!()
    }
}

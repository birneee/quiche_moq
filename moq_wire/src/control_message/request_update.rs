use crate::bytes::{FromBytes, ToBytes};
use crate::vi64;
use crate::{Parameters, RequestId, Version, REQUEST_UPDATE_MESSAGE_ID};
use crate::control_message::ControlMessage;
use octets::{Octets, OctetsMut};

#[derive(Debug)]
pub struct RequestUpdateMessage {
    pub request_id: RequestId,
    pub existing_request_id: RequestId,
    pub parameters: Parameters,
}

impl ControlMessage for RequestUpdateMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_UPDATE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_update" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        vi64::write_varint(b, self.request_id, version)?;
        vi64::write_varint(b, self.existing_request_id, version)?;
        self.parameters.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = vi64::read_varint(b, version)?;
        let existing_request_id = vi64::read_varint(b, version)?;
        let parameters = Parameters::from_bytes(b, version)?;
        Ok(Self { request_id, existing_request_id, parameters })
    }
}

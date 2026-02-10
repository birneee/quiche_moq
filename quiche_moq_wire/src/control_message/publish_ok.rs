use octets::{Octets, OctetsMut};
use crate::{FromBytes, Parameters, RequestId, ToBytes, Version, PUBLISH_OK_CONTROL_MESSAGE_ID};
use crate::control_message::ControlMessage;
use crate::Result;

#[derive(Debug)]
pub struct PublishOkMessage {
    request_id: RequestId,
    parameters: Parameters,
}

impl PublishOkMessage {
    pub fn new(request_id: RequestId, parameters: Parameters) -> Self {
        Self {
            request_id,
            parameters,
        }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn parameters(&self) -> &Parameters {
        &self.parameters
    }
}

impl ControlMessage for PublishOkMessage {
    fn message_id() -> u64 {
        PUBLISH_OK_CONTROL_MESSAGE_ID
    }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::Result<()> {
        b.put_u16(self.request_id as u16)?;
        self.parameters.to_bytes(b, version)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        let request_id = b.get_u16()? as RequestId;
        let parameters = Parameters::from_bytes(b, version)?;
        Ok(Self {
            request_id,
            parameters
        })
    }
}
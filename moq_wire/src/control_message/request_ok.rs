use crate::bytes::{FromBytes, ToBytes};
use crate::vi64;
use crate::{Parameters, RequestId, Version, REQUEST_OK_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::namespace::Namespace;

#[derive(Debug)]
/// Called ANNOUNCE_OK before draft-13
/// Called REQUEST_OK since draft-14
///
/// draft-17 (#1510) drops Request ID: correlation moves from an explicit ID to
/// the request's dedicated bidi stream (REQUEST_OK is "the first response message
/// on the bidi stream"). Callers on draft 17+ must track stream<->request_id
/// themselves; `parameters`/`track_properties` are the only content on the wire.
pub struct RequestOkMessage {
    /// Some for draft 7 to 10
    request_id: Option<RequestId>,
    /// Some for draft 11 to 13
    track_namespace: Option<Namespace>,
    /// draft 16+: Parameters (Section 10.2). Empty for most responses.
    parameters: Parameters,
    /// draft 18+: Track Properties (Section 2.5). Always empty except TRACK_STATUS_OK.
    track_properties: Parameters,
}

impl RequestOkMessage {
    pub fn new(request_id: Option<RequestId>, track_namespace: Option<Namespace>) -> Self {
        RequestOkMessage { request_id, track_namespace, parameters: Parameters(vec![]), track_properties: Parameters(vec![]) }
    }

    /// draft 17+: no Request ID on the wire; correlation is via the request's stream.
    pub fn new_stream_correlated(parameters: Parameters) -> Self {
        RequestOkMessage { request_id: None, track_namespace: None, parameters, track_properties: Parameters(vec![]) }
    }

    pub fn parameters(&self) -> &Parameters { &self.parameters }
}

impl ControlMessage for RequestOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_ok" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                self.track_namespace.as_ref().unwrap().to_bytes(b, version)?;
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_16 => {
                vi64::write_varint(b, self.request_id.unwrap(), version)?;
            }
            MOQ_VERSION_DRAFT_17..=MOQ_VERSION_DRAFT_18 => {
                self.parameters.to_bytes(b, version)?;
                if version == MOQ_VERSION_DRAFT_18 {
                    self.track_properties.to_bytes_no_count(b, version)?;
                }
            }
            _ => unimplemented!()
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
                let track_namespace = Some(Namespace::from_bytes(b, version)?);
                Ok(Self{
                    track_namespace,
                    request_id: None,
                    parameters: Parameters(vec![]),
                    track_properties: Parameters(vec![]),
                })
            }
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_16 => {
                let request_id = Some(vi64::read_varint(b, version)?);
                Ok(Self {
                    request_id,
                    track_namespace: None,
                    parameters: Parameters(vec![]),
                    track_properties: Parameters(vec![]),
                })
            }
            MOQ_VERSION_DRAFT_17..=MOQ_VERSION_DRAFT_18 => {
                let parameters = Parameters::from_bytes(b, version)?;
                let track_properties = if version == MOQ_VERSION_DRAFT_18 {
                    Parameters::from_bytes_until_exhausted(b, version)?
                } else {
                    Parameters(vec![])
                };
                Ok(Self { request_id: None, track_namespace: None, parameters, track_properties })
            }
            _ => unimplemented!()
        }
    }
}

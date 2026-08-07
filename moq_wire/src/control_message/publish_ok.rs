use octets::{Octets, OctetsMut};
use crate::vi64;
use crate::{DELIVERY_TIMEOUT_PARAMETER_ID, FromBytes, Location, Parameters, RequestId, ToBytes, Version,
            PUBLISH_OK_MESSAGE_ID, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16,
            MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
use crate::control_message::subscribe::FilterType;
use crate::control_message::ControlMessage;
use crate::Result;

/// PUBLISH_OK sent by the subscriber to accept a PUBLISH.
///
/// Draft-12..14: inline Forward(8), Subscriber Priority(8), Group Order(8),
///   Filter Type(i), optional Start(Location), optional EndGroup(i), Parameters.
/// Draft-15..16: Request ID(i) + Parameters.
/// Draft-17+ (#1510): Request ID is dropped from the wire — PUBLISH_OK is "the
///   first response message on the [PUBLISH request's] bidi stream", so callers
///   must track stream<->request_id themselves. `request_id` on this struct is
///   then meaningless on decode (set to 0) and ignored on encode.
#[derive(Debug, PartialEq)]
pub struct PublishOkMessage {
    pub request_id: RequestId,
    pub parameters: Parameters,
    /// draft 18+: Track Properties (Section 2.5). Always empty for PUBLISH_OK.
    pub track_properties: Parameters,
}

impl PublishOkMessage {
    pub fn new(request_id: RequestId, parameters: Parameters) -> Self {
        Self { request_id, parameters, track_properties: Parameters(vec![]) }
    }

    pub fn request_id(&self) -> RequestId { self.request_id }
    pub fn parameters(&self) -> &Parameters { &self.parameters }

    pub fn delivery_timeout(&self) -> Option<u64> {
        self.parameters.get_varint(DELIVERY_TIMEOUT_PARAMETER_ID)
    }
}

impl ControlMessage for PublishOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_ok" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::Result<()> {
        match version {
            // Draft-12..14: inline Forward, Subscriber Priority, Group Order, Filter Type
            // (these became Parameters in draft-15). We write defaults when encoding older formats.
            MOQ_VERSION_DRAFT_12..=0xff00000e /* draft-14 */ => {
                vi64::write_varint(b, self.request_id, version)?;
                b.put_u8(1)?; // Forward = 1 (forward)
                b.put_u8(128)?; // Subscriber Priority = 128 (default)
                b.put_u8(0)?; // Group Order = 0 (original publisher preference)
                FilterType::LargestObject.to_bytes(b, version)?;
                // LargestObject has no Start or EndGroup
            }
            MOQ_VERSION_DRAFT_15..=MOQ_VERSION_DRAFT_16 => {
                vi64::write_varint(b, self.request_id, version)?;
            }
            MOQ_VERSION_DRAFT_17..=MOQ_VERSION_DRAFT_18 => {} // no Request ID; correlated by stream
            _ => unimplemented!("PUBLISH_OK is only supported in draft-12+"),
        }
        self.parameters.to_bytes(b, version)?;
        if version == MOQ_VERSION_DRAFT_18 {
            self.track_properties.to_bytes_no_count(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> Result<Self> {
        let request_id = match version {
            // Draft-12..14: consume inline fields; they are ignored (not stored — these fields
            // became Parameters in draft-15+, so callers use parameters() instead).
            MOQ_VERSION_DRAFT_12..=0xff00000e /* draft-14 */ => {
                let request_id = vi64::read_varint(b, version)?;
                let _forward = b.get_u8()?;
                let _subscriber_priority = b.get_u8()?;
                let _group_order = b.get_u8()?;
                let filter_type = FilterType::from_bytes(b, version)?;
                if filter_type.has_start_location() {
                    Location::from_bytes(b, version)?;
                }
                if filter_type.has_end_group() {
                    vi64::read_varint(b, version)?;
                }
                request_id
            }
            MOQ_VERSION_DRAFT_15..=MOQ_VERSION_DRAFT_16 => vi64::read_varint(b, version)?,
            MOQ_VERSION_DRAFT_17..=MOQ_VERSION_DRAFT_18 => 0, // no Request ID; correlated by stream
            _ => unimplemented!("PUBLISH_OK is only supported in draft-12+"),
        };
        let parameters = Parameters::from_bytes(b, version)?;
        let track_properties = if version == MOQ_VERSION_DRAFT_18 {
            Parameters::from_bytes_until_exhausted(b, version)?
        } else {
            Parameters(vec![])
        };
        Ok(Self { request_id, parameters, track_properties })
    }
}

use crate::bytes::{FromBytes, ToBytes};
use crate::{
    DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, EXPIRES_PARAMETER_ID, LARGEST_OBJECT_PARAMETER_ID,
    Parameter, Parameters, RequestId, TrackAlias, Version,
    MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_15,
    MOQ_VERSION_DRAFT_16, SUBSCRIBE_OK_MESSAGE_ID,
};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::control_message::subscribe::SubscribeMessage;
use crate::error::Error;
use crate::location::Location;

#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeOkMessage {
    pub(crate) request_id: RequestId,
    /// `None` for draft 07 to draft 11
    track_alias: Option<TrackAlias>,
    parameters: SubscribeOkParameters,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GroupOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Eq, PartialEq)]
struct SubscribeOkParameters {
    expires: u64,
    group_order: GroupOrder,
    largest_location: Option<Location>,
    extra_parameters: Parameters,
}

impl SubscribeOkMessage {
    pub fn from(sm: &SubscribeMessage, track_alias: Option<TrackAlias>, largest_location: Option<Location>) -> Self {
        assert!(track_alias.is_none() ^ sm.track_alias.is_none());
        Self {
            request_id: sm.request_id,
            track_alias,
            parameters: SubscribeOkParameters {
                expires: 0,
                group_order: GroupOrder::Ascending,
                largest_location,
                extra_parameters: Parameters(vec![]),
            },
        }
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// `None` for draft 7 to draft 11
    pub fn track_alias(&self) -> Option<u64> {
        self.track_alias
    }

    pub fn expires(&self) -> u64 {
        self.parameters.expires
    }

    pub fn group_order(&self) -> GroupOrder {
        self.parameters.group_order
    }

    pub fn largest_location(&self) -> Option<Location> {
        self.parameters.largest_location
    }
}

impl ControlMessage for SubscribeOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe_ok" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.request_id)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {}
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_16 => {
                b.put_varint(self.track_alias.unwrap())?;
            }
            _ => unimplemented!()
        }
        self.parameters.to_bytes(b, version)
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = b.get_varint()?;
        let track_alias = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => None,
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_16 => Some(b.get_varint()?),
            _ => unimplemented!()
        };
        let parameters = SubscribeOkParameters::from_bytes(b, version)?;
        Ok(Self { request_id, track_alias, parameters })
    }
}

impl FromBytes for SubscribeOkParameters {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::Result<Self> {
        match version {
            // Drafts 07–15: inline fields followed by a trailing Parameters section.
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_15 => {
                let expires = b.get_varint()?;
                let group_order_byte = b.get_u8()?;
                if group_order_byte != 0x1 && group_order_byte != 0x2 {
                    return Err(Error::ProtocolViolation("invalid group order".into()));
                }
                let content_exists = b.get_u8()?;
                let largest_location = if content_exists == 1 {
                    Some(Location::from_bytes(b, version)?)
                } else {
                    None
                };
                let extra_parameters = Parameters::from_bytes(b, version)?;
                Ok(Self {
                    expires,
                    group_order: if group_order_byte == 2 { GroupOrder::Descending } else { GroupOrder::Ascending },
                    largest_location,
                    extra_parameters,
                })
            }
            // Draft 16: single count-prefixed Parameters section containing all KVPs.
            MOQ_VERSION_DRAFT_16 => {
                let params = Parameters::from_bytes(b, version)?;
                let expires = params.get_varint(EXPIRES_PARAMETER_ID).unwrap_or(0);
                let largest_location = params.get_bytes(LARGEST_OBJECT_PARAMETER_ID).and_then(|bytes| {
                    Location::from_bytes(&mut Octets::with_slice(bytes), 0).ok()
                });
                let group_order_val = params.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID).unwrap_or(1);
                let group_order = if group_order_val == 2 { GroupOrder::Descending } else { GroupOrder::Ascending };
                let known = [EXPIRES_PARAMETER_ID, LARGEST_OBJECT_PARAMETER_ID, DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID];
                let extra_parameters = Parameters(params.0.into_iter().filter(|p| !known.contains(&p.ty)).collect());
                Ok(Self { expires, group_order, largest_location, extra_parameters })
            }
            _ => unimplemented!()
        }
    }
}

impl ToBytes for SubscribeOkParameters {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::Result<()> {
        match version {
            // Drafts 07–15: inline fields followed by a trailing Parameters section.
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_15 => {
                b.put_varint(self.expires)?;
                b.put_u8(match self.group_order {
                    GroupOrder::Ascending => 1,
                    GroupOrder::Descending => 2,
                })?;
                if let Some(loc) = &self.largest_location {
                    let mut loc_buf = [0u8; 18];
                    let mut loc_o = OctetsMut::with_slice(&mut loc_buf);
                    loc.to_bytes(&mut loc_o, version)?;
                    let loc_len = loc_o.off();
                    b.put_u8(1)?;
                    b.put_bytes(&loc_buf[..loc_len])?;
                } else {
                    b.put_u8(0)?;
                }
                self.extra_parameters.to_bytes(b, version)
            }
            // Draft 16: single count-prefixed Parameters section with all KVPs.
            // Parameters::to_bytes handles sorting by type ID for delta-encoding.
            MOQ_VERSION_DRAFT_16 => {
                let group_order_val: u64 = match self.group_order {
                    GroupOrder::Ascending => 1,
                    GroupOrder::Descending => 2,
                };
                let mut params_vec = vec![];
                if self.expires != 0 {
                    params_vec.push(Parameter::new_varint(EXPIRES_PARAMETER_ID, self.expires));
                }
                if let Some(loc) = &self.largest_location {
                    let mut loc_buf = [0u8; 18];
                    let mut loc_o = OctetsMut::with_slice(&mut loc_buf);
                    loc.to_bytes(&mut loc_o, 0)?;
                    let loc_len = loc_o.off();
                    params_vec.push(Parameter::new_bytes(LARGEST_OBJECT_PARAMETER_ID, loc_buf[..loc_len].to_vec()));
                }
                params_vec.push(Parameter::new_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, group_order_val));
                params_vec.extend(self.extra_parameters.0.iter().cloned());
                Parameters(params_vec).to_bytes(b, version)
            }
            _ => unimplemented!()
        }
    }
}

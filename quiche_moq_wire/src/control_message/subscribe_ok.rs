use crate::bytes::{FromBytes, ToBytes};
use crate::{
    DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, EXPIRES_PARAMETER_ID, LARGEST_OBJECT_PARAMETER_ID,
    Parameter, Parameters, RequestId, TrackAlias, Version,
    MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13,
    MOQ_VERSION_DRAFT_16, SUBSCRIBE_OK_MESSAGE_ID,
};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::control_message::subscribe::SubscribeMessage;
use crate::error::Error;
use crate::key_value_pair::KvpCtx;
use crate::location::Location;

/// Parameter type IDs that are encoded as inline fields in old drafts (07–13) rather than as
/// standard trailing parameters. When re-encoding old drafts these are written inline; the
/// remaining parameters are written via the trailing Parameters section.
const INLINE_PARAM_IDS: &[u64] = &[
    EXPIRES_PARAMETER_ID,
    LARGEST_OBJECT_PARAMETER_ID,
    DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID,
];

#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeOkMessage {
    pub(crate) request_id: RequestId,
    /// `None` for draft 07 to draft 11
    track_alias: Option<TrackAlias>,
    /// Stores all parameters including expires (0x8), largest_location (0x9),
    /// and group_order (0x22). Use the getter methods for convenient access.
    parameters: Parameters,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum GroupOrder {
    Ascending,
    Descending,
}

impl SubscribeOkMessage {
    pub fn from(sm: &SubscribeMessage, track_alias: Option<TrackAlias>, largest_location: Option<Location>) -> Self {
        assert!(track_alias.is_none() ^ sm.track_alias.is_none());
        let mut parameters = Parameters(vec![]);
        // Default group order: Ascending (0x1)
        parameters.0.push(Parameter::new_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, 0x1));
        if let Some(loc) = largest_location {
            let mut loc_buf = [0u8; 18];
            let mut loc_o = OctetsMut::with_slice(&mut loc_buf);
            loc.to_bytes(&mut loc_o, 0).unwrap();
            let loc_len = loc_o.off();
            parameters.0.push(Parameter::new_bytes(LARGEST_OBJECT_PARAMETER_ID, loc_buf[..loc_len].to_vec()));
        }
        Self {
            request_id: sm.request_id,
            track_alias,
            parameters,
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
        self.parameters.get_varint(EXPIRES_PARAMETER_ID).unwrap_or(0)
    }

    pub fn group_order(&self) -> GroupOrder {
        match self.parameters.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID).unwrap_or(1) {
            2 => GroupOrder::Descending,
            _ => GroupOrder::Ascending,
        }
    }

    pub fn largest_location(&self) -> Option<Location> {
        let bytes = self.parameters.get_bytes(LARGEST_OBJECT_PARAMETER_ID)?;
        let mut oct = Octets::with_slice(bytes);
        Location::from_bytes(&mut oct, 0).ok()
    }

    /// Returns a `Parameters` containing only the parameters that are not encoded inline in
    /// old drafts (i.e. excluding expires, largest_location, and group_order).
    fn trailing_parameters(&self) -> Parameters {
        Parameters(
            self.parameters.0.iter()
                .filter(|p| !INLINE_PARAM_IDS.contains(&p.ty))
                .cloned()
                .collect()
        )
    }
}

impl ControlMessage for SubscribeOkMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_OK_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe_ok" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(self.request_id)?;
        match version {
            // Drafts 07–13: expires/group_order/largest_location are standalone inline fields
            // followed by a standard trailing Parameters section.
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
                b.put_varint(self.expires())?;
                b.put_u8(self.parameters.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID).unwrap_or(1) as u8)?;
                if let Some(loc_bytes) = self.parameters.get_bytes(LARGEST_OBJECT_PARAMETER_ID) {
                    b.put_u8(1)?;
                    b.put_bytes(loc_bytes)?;
                } else {
                    b.put_u8(0)?;
                }
                self.trailing_parameters().to_bytes(b, version)?;
            }
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {
                b.put_varint(self.track_alias.unwrap())?;
                b.put_varint(self.expires())?;
                b.put_u8(self.parameters.get_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID).unwrap_or(1) as u8)?;
                if let Some(loc_bytes) = self.parameters.get_bytes(LARGEST_OBJECT_PARAMETER_ID) {
                    b.put_u8(1)?;
                    b.put_bytes(loc_bytes)?;
                } else {
                    b.put_u8(0)?;
                }
                self.trailing_parameters().to_bytes(b, version)?;
            }
            // Draft 16: everything moves into Parameters (count-prefixed, delta-encoded) and
            // Track Extensions (count-less, delta-encoded).
            MOQ_VERSION_DRAFT_16 => {
                b.put_varint(self.track_alias.unwrap())?;

                // Parameters section: EXPIRES (0x8) and LARGEST_OBJECT (0x9)
                let params_section = Parameters(
                    self.parameters.0.iter()
                        .filter(|p| p.ty == EXPIRES_PARAMETER_ID || p.ty == LARGEST_OBJECT_PARAMETER_ID)
                        .cloned()
                        .collect()
                );
                params_section.to_bytes(b, version)?;

                // Track Extensions section (count-less): DEFAULT_PUBLISHER_GROUP_ORDER (0x22)
                let mut prev_key = 0u64;
                for p in self.parameters.0.iter().filter(|p| p.ty == DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID) {
                    p.to_bytes(b, KvpCtx::new(version).with_previous_key(prev_key))?;
                    prev_key = p.ty;
                }
            }
            _ => unimplemented!()
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = b.get_varint()?;
        let mut parameters = Parameters(vec![]);
        match version {
            // Drafts 07–13: parse inline fields and convert to Parameters entries.
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
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
                let rest_params = Parameters::from_bytes(b, version)?;

                if expires != 0 {
                    parameters.0.push(Parameter::new_varint(EXPIRES_PARAMETER_ID, expires));
                }
                parameters.0.push(Parameter::new_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, group_order_byte as u64));
                if let Some(loc) = largest_location {
                    let mut loc_buf = [0u8; 18];
                    let mut loc_o = OctetsMut::with_slice(&mut loc_buf);
                    loc.to_bytes(&mut loc_o, version)?;
                    let loc_len = loc_o.off();
                    parameters.0.push(Parameter::new_bytes(LARGEST_OBJECT_PARAMETER_ID, loc_buf[..loc_len].to_vec()));
                }
                parameters.0.extend(rest_params.0);
                Ok(Self { request_id, track_alias: None, parameters })
            }
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {
                let track_alias = Some(b.get_varint()?);
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
                let rest_params = Parameters::from_bytes(b, version)?;

                if expires != 0 {
                    parameters.0.push(Parameter::new_varint(EXPIRES_PARAMETER_ID, expires));
                }
                parameters.0.push(Parameter::new_varint(DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID, group_order_byte as u64));
                if let Some(loc) = largest_location {
                    let mut loc_buf = [0u8; 18];
                    let mut loc_o = OctetsMut::with_slice(&mut loc_buf);
                    loc.to_bytes(&mut loc_o, version)?;
                    let loc_len = loc_o.off();
                    parameters.0.push(Parameter::new_bytes(LARGEST_OBJECT_PARAMETER_ID, loc_buf[..loc_len].to_vec()));
                }
                parameters.0.extend(rest_params.0);
                Ok(Self { request_id, track_alias, parameters })
            }
            // Draft 16: read Parameters section (count+delta KVPs) then Track Extensions
            // (count-less delta KVPs until end of message body).
            MOQ_VERSION_DRAFT_16 => {
                let track_alias = Some(b.get_varint()?);

                // Parameters section (count-prefixed, delta-encoded by Parameters::from_bytes)
                let params_section = Parameters::from_bytes(b, version)?;
                parameters.0.extend(params_section.0);

                // Track Extensions (remaining bytes, delta-encoded from 0)
                let mut prev_key = 0u64;
                while b.cap() > 0 {
                    let p = Parameter::from_bytes(b, KvpCtx::new(version).with_previous_key(prev_key))?;
                    prev_key = p.ty;
                    parameters.0.push(p);
                }

                Ok(Self { request_id, track_alias, parameters })
            }
            _ => unimplemented!()
        }
    }
}

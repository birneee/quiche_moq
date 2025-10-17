use crate::bytes::{FromBytes, ToBytes};
use crate::control_message::set_control_message_length;
use crate::{Parameters, RequestId, TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, SUBSCRIBE_OK_CONTROL_MESSAGE_ID};
use octets::{Octets, OctetsMut};
use crate::control_message::header::ControlMessageHeader;
use crate::control_message::subscribe::SubscribeMessage;
use crate::error::Error;
use crate::location::Location;

#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeOkMessage {
    pub(crate) request_id: RequestId,
    /// `None` for draft 07 to draft 11
    track_alias: Option<TrackAlias>,
    expires: u64,
    group_order: GroupOrder,
    largest_location: Option<Location>,
    parameters: Parameters,
}

impl SubscribeOkMessage {

    pub fn from(sm: &SubscribeMessage, track_alias: Option<TrackAlias>) -> Self {
        assert!(track_alias.is_none() ^ sm.track_alias.is_none());
        Self {
            request_id: sm.request_id,
            track_alias,
            expires: 0,
            group_order: GroupOrder::Ascending,
            largest_location: None,
            parameters: Parameters(vec![])
        }
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// `None` for draft 7 to draft 11
    pub fn track_alias(&self) -> Option<u64> {
        self.track_alias
    }
}

impl FromBytes for SubscribeOkMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty, SUBSCRIBE_OK_CONTROL_MESSAGE_ID);
        assert!(b.cap() >= header.len as usize);
        let request_id = b.get_varint()?;
        let track_alias = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => None,
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => Some(b.get_varint()?),
            _ => unimplemented!(),
        };
        let expires = b.get_varint()?;
        let group_order = GroupOrder::from_bytes(b, version)?;
        let content_exists = b.get_u8()?;
        let largest_location = if content_exists == 1 {
            Some(Location::from_bytes(b, version)?)
        } else {
            None
        };
        let parameters = Parameters::from_bytes(b, version)?;

        Ok(Self{
            request_id,
            track_alias,
            expires,
            group_order,
            largest_location,
            parameters,
        })
    }
}

impl ToBytes for SubscribeOkMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        b.put_varint(SUBSCRIBE_OK_CONTROL_MESSAGE_ID)?;
        let len_off = b.off();
        b.skip(2)?;
        b.put_varint(self.request_id)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {}, //no track alias
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {
                b.put_varint(self.track_alias.unwrap())?;
            },
            _ => unimplemented!()
        }
        b.put_varint(self.expires)?;
        self.group_order.to_bytes(b, version)?;
        if let Some(location) = &self.largest_location {
            b.put_u8(1)?;
            location.to_bytes(b, version)?;
        } else {
            b.put_u8(0)?;
        }
        self.parameters.to_bytes(b, version)?;
        set_control_message_length(b, len_off, version)?;
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
enum GroupOrder {
    Ascending,
    Descending,
}

impl ToBytes for GroupOrder {
    fn to_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        match self {
            GroupOrder::Ascending => b.put_u8(0x1)?,
            GroupOrder::Descending => b.put_u8(0x2)?,
        };
        Ok(())
    }
}

impl FromBytes for GroupOrder {
    fn from_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        Ok(match b.get_u8()? {
            0x1 => GroupOrder::Ascending,
            0x2 => GroupOrder::Descending,
            _ => return Err(Error::ProtocolViolation("invalid group order".into())),
        })
    }
}

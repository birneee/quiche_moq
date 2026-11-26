use crate::prelude::*;
use crate::error::{Error, Result};

/// How to prioritize the groups of a subscription (draft-ietf-moq-transport-16 §9.2.2.4).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GroupOrder {
    Ascending = 1,
    Descending = 2,
}

impl GroupOrder {
    /// The inline field until draft 14, 0 for the publisher's order.
    pub(crate) fn from_requested(id: u8) -> Result<Option<Self>> {
        match id {
            0 => Ok(None),
            id => Self::try_from(u64::from(id)).map(Some),
        }
    }

    pub(crate) fn requested_id(order: Option<Self>) -> u8 {
        order.map_or(0, |order| order as u8)
    }
}

impl From<GroupOrder> for u64 {
    fn from(order: GroupOrder) -> Self {
        order as u64
    }
}

impl TryFrom<u64> for GroupOrder {
    type Error = Error;

    fn try_from(id: u64) -> Result<Self> {
        match id {
            1 => Ok(Self::Ascending),
            2 => Ok(Self::Descending),
            _ => Err(Error::ProtocolViolation(format!("group order {id}"))),
        }
    }
}

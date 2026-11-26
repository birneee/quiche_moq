use crate::prelude::*;
use crate::error::{Error, Result};

/// not used after draft7
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Role {
    Publisher,
    Subscriber,
    PubSub,
}

impl Role {
    pub fn from_id(v: u64) -> Result<Self> {
        Ok(match v {
            0x01 => Self::Publisher,
            0x02 => Self::Subscriber,
            0x03 => Self::PubSub,
            _ => return Err(Error::ProtocolViolation("unexpected role".to_string()))
        })
    }

    pub fn to_id(&self) -> u64 {
        match self {
            Role::Publisher => 0x01,
            Role::Subscriber => 0x02,
            Role::PubSub => 0x03,
        }
    }
}

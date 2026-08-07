use octets::{Octets, OctetsMut};
use crate::vi64;
use crate::version;
use crate::{ReasonPhrase, RequestId, TrackAlias, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18, REQUEST_ERROR_MESSAGE_ID};
use crate::control_message::{ControlMessage, SubscribeMessage};

#[allow(unused)]
#[derive(Debug)]
/// Called SUBSCRIBE_ERROR before draft-13
/// Called REQUEST_ERROR since draft-14
pub struct RequestErrorMessage {
    /// formerly known as subscribe ID
    request_id: RequestId,
    error_code: u64,
    error_reason: ReasonPhrase,
    /// only present from draft 07 to draft 11
    track_alias: Option<TrackAlias>,
    /// only present since draft 16; value+1 in ms, 0 means do not retry
    retry_interval: Option<u64>,
}

impl RequestErrorMessage {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
    pub fn error_code(&self) -> u64 { self.error_code }
    pub fn error_reason(&self) -> &ReasonPhrase {
        &self.error_reason
    }
    pub fn retry_interval(&self) -> Option<u64> {
        self.retry_interval
    }
    pub fn new(request_id: RequestId, error_code: u64) -> Self {
        Self {
            request_id,
            error_code,
            error_reason: ReasonPhrase("".to_string()),
            track_alias: None,
            retry_interval: None,
        }
    }

    pub fn from(sm: &SubscribeMessage, error_code: u64) -> Self {
        Self {
            request_id: sm.request_id,
            error_code,
            error_reason: ReasonPhrase("".to_string()),
            track_alias: sm.track_alias,
            retry_interval: None,
        }
    }
}

impl std::fmt::Display for RequestErrorMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.error_code;
        let name = match code {
            0x00 => "InternalError",
            0x01 => "Unauthorized",
            0x02 => "Timeout",
            0x03 => "NotSupported",
            0x04 => "MalformedAuthToken",
            0x05 => "ExpiredAuthToken",
            0x10 => "DoesNotExist",
            0x11 => "InvalidRange",
            0x12 => "MalformedTrack",
            0x19 => "DuplicateSubscription",
            0x20 => "Uninterested",
            0x30 => "PrefixOverlap",
            0x32 => "JoiningRequestId",
            other => return write!(f, "0x{other:x}"),
        };
        write!(f, "{name} (0x{code:x})")?;
        let reason = self.error_reason.0.as_str();
        if !reason.is_empty() { write!(f, ": {reason}")?; }
        Ok(())
    }
}

impl ControlMessage for RequestErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_ERROR_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_error" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        if !version::stream_per_request(version) {
            vi64::write_varint(b, self.request_id, version)?;
        }
        vi64::write_varint(b, self.error_code, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
                self.error_reason.to_bytes(b, version)?;
                vi64::write_varint(b, self.track_alias.unwrap(), version)?;
            },
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_15 => {
                self.error_reason.to_bytes(b, version)?;
            },
            MOQ_VERSION_DRAFT_16..=MOQ_VERSION_DRAFT_18 => {
                vi64::write_varint(b, self.retry_interval.unwrap_or(0), version)?;
                self.error_reason.to_bytes(b, version)?;
            },
            _ => unimplemented!(),
        };
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = if version::stream_per_request(version) {
            0 // no Request ID on the wire; correlated by stream
        } else {
            vi64::read_varint(b, version)?
        };
        let error_code = vi64::read_varint(b, version)?;
        let (error_reason, track_alias, retry_interval) = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
                let reason = ReasonPhrase::from_bytes(b, version)?;
                let alias = vi64::read_varint(b, version)?;
                (reason, Some(alias), None)
            },
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_15 => {
                (ReasonPhrase::from_bytes(b, version)?, None, None)
            },
            MOQ_VERSION_DRAFT_16..=MOQ_VERSION_DRAFT_18 => {
                let retry = vi64::read_varint(b, version)?;
                let reason = ReasonPhrase::from_bytes(b, version)?;
                (reason, None, Some(retry))
            },
            _ => unimplemented!(),
        };
        Ok(Self {
            request_id,
            error_code,
            error_reason,
            track_alias,
            retry_interval,
        })
    }
}

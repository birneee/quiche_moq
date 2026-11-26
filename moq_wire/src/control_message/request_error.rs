use crate::prelude::*;
use crate::bytes::{FromBytes, ToBytes};
use crate::{Frame, Mode, NamespaceTuple, Reader, Writer};
use crate::{ReasonPhrase, RequestErrorCode, RequestId, TrackAlias, Version, REQUEST_ERROR_MESSAGE_ID};
use crate::control_message::{required, ControlMessage};

/// Where to retry a request (draft-ietf-moq-transport-19 §10.6.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redirect {
    /// empty for the current session's
    pub connect_uri: Vec<u8>,
    /// no fields and an empty `track_name` for the original request's
    pub namespace: NamespaceTuple,
    /// empty for namespace-scoped requests
    pub track_name: Vec<u8>,
}

impl FromBytes for Redirect {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> crate::error::Result<Self> {
        Ok(Self {
            connect_uri: r.length_prefixed_bytes()?.to_vec(),
            namespace: NamespaceTuple::from_bytes(r, version)?,
            track_name: r.length_prefixed_bytes()?.to_vec(),
        })
    }
}

impl ToBytes for Redirect {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> crate::error::Result<()> {
        w.length_prefixed_bytes(&self.connect_uri)?;
        self.namespace.to_bytes(w, version)?;
        Ok(w.length_prefixed_bytes(&self.track_name)?)
    }
}

#[derive(Debug)]
/// Called SUBSCRIBE_ERROR before draft-13
/// Called REQUEST_ERROR since draft-14
pub struct RequestErrorMessage {
    /// formerly known as subscribe ID
    pub(crate) request_id: RequestId,
    error_code: RequestErrorCode,
    error_reason: ReasonPhrase,
    /// only present from draft 07 to draft 11
    track_alias: Option<TrackAlias>,
    /// only present since draft 16; value+1 in ms, 0 means do not retry
    retry_interval: Option<u64>,
    /// draft 18 on, with error code REDIRECT
    redirect: Option<Redirect>,
}

impl RequestErrorMessage {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }
    pub fn error_code(&self) -> RequestErrorCode { self.error_code }
    pub fn error_reason(&self) -> &ReasonPhrase {
        &self.error_reason
    }
    pub fn retry_interval(&self) -> Option<u64> {
        self.retry_interval
    }
    pub fn redirect(&self) -> Option<&Redirect> {
        self.redirect.as_ref()
    }
    /// Drafts 7 to 11 only.
    pub fn track_alias(&self) -> Option<TrackAlias> {
        self.track_alias
    }
    pub fn new(request_id: RequestId, error_code: RequestErrorCode) -> Self {
        Self {
            request_id,
            error_code,
            error_reason: ReasonPhrase::default(),
            track_alias: None,
            retry_interval: None,
            redirect: None,
        }
    }

    pub fn with_reason(self, error_reason: ReasonPhrase) -> Self {
        Self { error_reason, ..self }
    }

    /// Drafts 7–11 name the rejected SUBSCRIBE's track alias.
    pub fn with_track_alias(self, track_alias: Option<TrackAlias>) -> Self {
        Self { track_alias, ..self }
    }

    /// Where to retry, sent with error code REDIRECT from draft 18.
    pub fn with_redirect(self, redirect: Redirect) -> Self {
        Self { redirect: Some(redirect), ..self }
    }

    fn has_redirect(error_code: RequestErrorCode, version: Version) -> bool {
        version.draft() >= 18 && error_code == RequestErrorCode::Redirect
    }
}

impl core::fmt::Display for RequestErrorMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.error_code)?;
        let reason: &str = &self.error_reason;
        if !reason.is_empty() { write!(f, ": {reason}")?; }
        Ok(())
    }
}

impl ControlMessage for RequestErrorMessage {
    const MESSAGE_IDS: &'static [u64] = &[REQUEST_ERROR_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "request_error" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        if !version.stream_per_request() {
            b.varint(self.request_id.0)?;
        }
        b.varint(self.error_code.into())?;
        match version.draft() {
            ..=11 => {
                self.error_reason.encode(b)?;
                b.varint(required(self.track_alias, "the track alias")?.0)?;
            },
            12..=15 => {
                self.error_reason.encode(b)?;
            },
            16.. => {
                b.varint(self.retry_interval.unwrap_or(0))?;
                self.error_reason.encode(b)?;
            },
        };
        if Self::has_redirect(self.error_code, version) {
            required(self.redirect.as_ref(), "the Redirect of a REDIRECT")?.to_bytes(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        let request_id = if version.stream_per_request() {
            RequestId(0) // no Request ID on the wire; correlated by stream
        } else {
            RequestId(b.varint()?)
        };
        let error_code = RequestErrorCode::from(b.varint()?);
        let (error_reason, track_alias, retry_interval) = match version.draft() {
            ..=11 => {
                let reason = ReasonPhrase::decode(b)?;
                let alias = TrackAlias(b.varint()?);
                (reason, Some(alias), None)
            },
            12..=15 => {
                (ReasonPhrase::decode(b)?, None, None)
            },
            16.. => {
                let retry = b.varint()?;
                let reason = ReasonPhrase::decode(b)?;
                (reason, None, Some(retry))
            },
        };
        let redirect = Self::has_redirect(error_code, version).then(|| Redirect::from_bytes(b, version)).transpose()?;
        Ok(Self {
            request_id,
            error_code,
            error_reason,
            track_alias,
            retry_interval,
            redirect,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::{from_slice, to_vec};
    use crate::{MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_19};

    /// draft-ietf-moq-transport-16 §1.4.3: a longer reason phrase is a PROTOCOL_VIOLATION.
    #[test]
    fn reason_phrase_over_1024_bytes_is_protocol_violation() {
        let version = MOQ_VERSION_DRAFT_16;
        let mut body = vec![];
        let mut w = version.writer(&mut body);
        // request id, error code, retry interval, reason phrase
        for field in [0, 0, 0] {
            w.varint(field).unwrap();
        }
        w.length_prefixed_bytes(&[b'x'; 1025]).unwrap();
        let decoded = RequestErrorMessage::from_body_bytes(&mut version.frame_reader(&body), version);
        assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
    }

    /// draft-ietf-moq-transport-19 §10.6.2: a REDIRECT ends with where to retry.
    #[test]
    fn draft_19_redirect() {
        let version = MOQ_VERSION_DRAFT_19;
        // type, length, REDIRECT, retry interval, reason | Connect URI, namespace `n`, track name `t`
        let bytes = [0x05, 0, 9, 0x34, 0, 0, 0, 1, 1, b'n', 1, b't'];
        let redirect = Redirect { connect_uri: vec![], namespace: NamespaceTuple::new(vec![b"n".to_vec()]).unwrap(), track_name: b"t".to_vec() };
        let error = RequestErrorMessage::new(RequestId(0), RequestErrorCode::Redirect).with_redirect(redirect.clone());
        assert_eq!(to_vec(&error, version).unwrap(), bytes);
        assert_eq!(from_slice::<RequestErrorMessage>(&bytes, version).unwrap().redirect(), Some(&redirect));
    }
}

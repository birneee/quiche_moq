use crate::{Frame, Reader, Writer};
use crate::{PublishDoneStatus, ReasonPhrase, RequestId, Version, PUBLISH_DONE_MESSAGE_ID, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_17};
use crate::control_message::ControlMessage;

#[derive(Debug)]
/// Called SUBSCRIBE_DONE before draft-13
/// Called PUBLISH_DONE since draft-14
/// - draft 07: no Stream Count, ContentExists and the final Location instead
/// - draft 17+: no Request ID, correlated by the request's stream (decoded as 0)
pub struct PublishDoneMessage {
    pub(crate) request_id: RequestId,
    status: PublishDoneStatus,
    stream_count: u64,
    error_reason: ReasonPhrase,
}

impl PublishDoneMessage {
    pub fn request_id(&self) -> RequestId { self.request_id }
    pub fn status(&self) -> PublishDoneStatus { self.status }
    pub fn stream_count(&self) -> u64 { self.stream_count }
    pub fn error_reason(&self) -> &ReasonPhrase { &self.error_reason }

    pub fn new(request_id: RequestId, status: PublishDoneStatus, stream_count: u64) -> Self {
        Self {
            request_id,
            status,
            stream_count,
            error_reason: ReasonPhrase::default(),
        }
    }
}

impl ControlMessage for PublishDoneMessage {
    const MESSAGE_IDS: &'static [u64] = &[PUBLISH_DONE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "publish_done" }

    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
        if version < MOQ_VERSION_DRAFT_17 {
            b.varint(self.request_id.0)?;
        }
        b.varint(self.status.into())?;
        if version == MOQ_VERSION_DRAFT_07 {
            self.error_reason.encode(b)?;
            b.u8(0); // ContentExists: the final Location isn't tracked
            return Ok(());
        }
        b.varint(self.stream_count)?;
        self.error_reason.encode(b)?;
        Ok(())
    }

    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self> {
        let request_id = RequestId(if version < MOQ_VERSION_DRAFT_17 { b.varint()? } else { 0 });
        let status = PublishDoneStatus::from(b.varint()?);
        if version == MOQ_VERSION_DRAFT_07 {
            let error_reason = ReasonPhrase::decode(b)?;
            if b.u8()? != 0 {
                let _final_group = b.varint()?;
                let _final_object = b.varint()?;
            }
            // unknown, the subscriber can't wait for streams
            return Ok(Self { request_id, status, stream_count: 0, error_reason });
        }
        let stream_count = b.varint()?;
        let error_reason = ReasonPhrase::decode(b)?;
        Ok(Self{
            request_id,
            status,
            stream_count,
            error_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07, 7, 0)]
    #[case(MOQ_VERSION_DRAFT_08, 7, 3)]
    #[case(MOQ_VERSION_DRAFT_16, 7, 3)]
    #[case(MOQ_VERSION_DRAFT_18, 0, 3)]
    fn roundtrip(#[case] version: Version, #[case] request_id: u64, #[case] stream_count: u64) {
        let msg = PublishDoneMessage::new(RequestId(7), PublishDoneStatus::TrackEnded, 3);
        let mut buf = vec![];
        msg.to_body_bytes(&mut version.writer(&mut buf), version).unwrap();
        let mut o = version.frame_reader(&buf);
        let decoded = PublishDoneMessage::from_body_bytes(&mut o, version).unwrap();
        o.finish().expect("whole body consumed");
        assert_eq!(decoded.request_id(), RequestId(request_id));
        assert_eq!(decoded.status(), PublishDoneStatus::TrackEnded);
        assert_eq!(decoded.stream_count(), stream_count);
    }
}

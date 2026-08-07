use crate::error::{Error, Result};
use crate::transport::{StreamError, Transport};
use log::debug;
use octets::{BufferTooShortError, Octets};
use moq_wire::object::ObjectHeader;
use moq_wire::subgroup::SubgroupHeader;
use moq_wire::{FromBytes, Version, PADDING_STREAM_TYPE_ID};
use short_buf::ShortBuf;
use std::cmp::min;

// Must be large enough to hold the complete serialised object header in one fill.
// LOC VideoConfig alone can be ~200 bytes; 4096 gives headroom.
const BUF_LEN: usize = 4096;

// Matches the chunk size `OutPaddingStream` sends per call.
const PADDING_DRAIN_BUF_LEN: usize = 1 << 16;

enum State {
    /// Stream type has not been read yet.
    Type,
    /// Subgroup header not yet read.
    SubgroupHeader,
    /// Ready subgroup stream.
    Subgroup {
        subgroup_header: Option<SubgroupHeader>,
        remaining_object_payload: usize,
        current_object_id: Option<u64>,
    },
    /// Padding stream — data is discarded.
    Padding,
}

/// Outcome of [`InStream::handle`].
pub enum Handled<'a> {
    /// This is a padding stream; one chunk was just discarded.
    Padding,
    /// The subgroup header is available.
    Subgroup(&'a SubgroupHeader),
}

pub struct InStream {
    stream_id: u64,
    version: Version,
    state: State,
    buf: ShortBuf<BUF_LEN>,
}

impl InStream {
    pub fn new(stream_id: u64, version: Version) -> Self {
        Self {
            stream_id,
            version,
            state: State::Type,
            buf: ShortBuf::new(),
        }
    }

    /// Drives this stream's state machine. Returns [`StreamError::Done`] when no
    /// progress is possible right now, or [`StreamError::Fin`] /
    /// [`StreamError::StreamReset`] when the stream is done.
    pub fn handle<T: Transport>(&mut self, t: &mut T) -> std::result::Result<Handled<'_>, StreamError> {
        loop {
            match self.state {
                State::Type => self.poll_type(t)?,
                State::Padding => {
                    self.drain_padding(t)?;
                    return Ok(Handled::Padding);
                }
                State::SubgroupHeader => self.poll_subgroup_header(t)?,
                State::Subgroup { .. } => {
                    let State::Subgroup { subgroup_header, .. } = &self.state else { unreachable!() };
                    return Ok(Handled::Subgroup(subgroup_header.as_ref().unwrap()));
                }
            }
        }
    }

    fn poll_type<T: Transport>(&mut self, t: &mut T) -> std::result::Result<(), StreamError> {
        loop {
            let mut b = Octets::with_slice(self.buf.buffer());
            match b.get_varint() {
                Ok(ty) => {
                    if ty == PADDING_STREAM_TYPE_ID {
                        debug!("stream {} is a padding stream", self.stream_id);
                        self.buf.consume(b.off());
                        self.state = State::Padding;
                    } else {
                        // Leave type bytes unconsumed; SubgroupHeader::from_bytes reads the type.
                        self.state = State::SubgroupHeader;
                    }
                    return Ok(());
                }
                Err(BufferTooShortError) => {
                    match self.buf.fill(|b| t.recv_stream(self.stream_id, b)) {
                        Ok(_) => {}
                        Err(StreamError::Done) => return Err(StreamError::Done),
                        Err(StreamError::Fin) | Err(StreamError::InvalidStreamState(_)) => {
                            return Err(StreamError::Done)
                        }
                        Err(e @ StreamError::StreamReset(_)) => return Err(e),
                        Err(e) => unimplemented!("{:?}", e),
                    }
                }
            }
        }
    }

    fn drain_padding<T: Transport>(&mut self, t: &mut T) -> std::result::Result<(), StreamError> {
        debug_assert!(matches!(self.state, State::Padding));
        let mut scratch = [0u8; PADDING_DRAIN_BUF_LEN];
        loop {
            match self.buf.chain_read2(|b| t.recv_stream(self.stream_id, b), &mut scratch) {
                Ok(_) => {}
                Err(StreamError::Done) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn poll_subgroup_header<T: Transport>(&mut self, t: &mut T) -> std::result::Result<(), StreamError> {
        let h = loop {
            let mut b = Octets::with_slice(self.buf.buffer());
            match SubgroupHeader::from_bytes(&mut b, self.version) {
                Ok(h) => {
                    self.buf.consume(b.off());
                    break h;
                }
                Err(moq_wire::Error::Octets(BufferTooShortError)) => {
                    match self.buf.fill(|b| t.recv_stream(self.stream_id, b)) {
                        Ok(_) => {}
                        Err(StreamError::Done) => return Err(StreamError::Done),
                        Err(e @ StreamError::StreamReset(_)) => return Err(e),
                        Err(StreamError::Fin) | Err(StreamError::InvalidStreamState(_)) => {
                            return Err(StreamError::Done)
                        }
                        Err(e) => unimplemented!("{:?}", e),
                    }
                }
                Err(e) => unimplemented!("{:?}", e),
            }
        };

        debug!("parsed subgroup header: {:?}", h);
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
                name: "moqt:subgroup_header_parsed".into(),
                data: serde_json::json!({
                    "stream_id": self.stream_id,
                    "track_alias": h.track_alias(),
                    "group_id": h.group_id(),
                    "subgroup_id": h.subgroup_id(),
                    "publisher_priority": h.publisher_priority(),
                }),
            }).ok();
        }
        self.state = State::Subgroup {
            subgroup_header: Some(h),
            remaining_object_payload: 0,
            current_object_id: None,
        };
        Ok(())
    }

    fn read_next_object_header<T: Transport>(&mut self, t: &mut T) -> Result<ObjectHeader> {
        let State::Subgroup { subgroup_header, remaining_object_payload, .. } = &self.state
        else {
            unreachable!()
        };
        assert_eq!(*remaining_object_payload, 0);
        let subgroup_header = subgroup_header.as_ref().unwrap();

        let object_header = loop {
            let mut b = Octets::with_slice(self.buf.buffer());
            let oh = match ObjectHeader::from_bytes(&mut b, self.version, subgroup_header) {
                Ok(v) => v,
                Err(moq_wire::Error::Octets(octets::BufferTooShortError)) => {
                    match self.buf.fill(|b| t.recv_stream(self.stream_id, b)) {
                        Ok(_) => {}
                        Err(StreamError::Done) => return Err(Error::Done),
                        Err(StreamError::Fin)
                        | Err(StreamError::StreamReset(_))
                        | Err(StreamError::InvalidStreamState(_)) => return Err(Error::Fin),
                        Err(e) => unimplemented!("{:?}", e),
                    }
                    continue;
                }
                Err(e) => unimplemented!("{:?}", e),
            };
            self.buf.consume(b.off());
            break oh;
        };

        debug!("parsed object header: {:?}", object_header);
        let State::Subgroup { remaining_object_payload, current_object_id, .. } = &mut self.state
        else {
            unreachable!()
        };
        *remaining_object_payload = object_header.payload_len();
        *current_object_id = Some(object_header.id());
        Ok(object_header)
    }

    pub fn read_obj_hdr<T: Transport>(&mut self, t: &mut T) -> Result<ObjectHeader> {
        let header = self.read_next_object_header(t)?;
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            let sh = match &self.state {
                State::Subgroup { subgroup_header, .. } => subgroup_header.as_ref().unwrap(),
                _ => unreachable!(),
            };
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
                name: "moqt:subgroup_object_parsed".into(),
                data: serde_json::json!({
                    "stream_id": self.stream_id,
                    "group_id": sh.group_id(),
                    "subgroup_id": sh.subgroup_id(),
                    "object_id": header.id(),
                    "extension_headers_length": header.extension_headers_len() as u64,
                    "extension_headers": header.extension_headers_to_qlog(),
                    "object_payload_length": header.payload_len() as u64,
                    "object_status": header.status(),
                }),
            }).ok();
        }
        Ok(header)
    }

    /// 0 if the next object header can be read.
    pub fn remaining_object_payload(&self) -> usize {
        match &self.state {
            State::Subgroup { remaining_object_payload, .. } => *remaining_object_payload,
            _ => 0,
        }
    }

    pub fn subgroup_header(&self) -> Option<&SubgroupHeader> {
        match &self.state {
            State::Subgroup { subgroup_header, .. } => subgroup_header.as_ref(),
            _ => None,
        }
    }

    /// Returns [`Error::Done`] when no data is available right now.
    pub fn read_obj_pld<T: Transport>(&mut self, t: &mut T, buf: &mut [u8]) -> Result<usize> {
        let State::Subgroup { remaining_object_payload, .. } = &self.state else { unreachable!() };
        assert_ne!(*remaining_object_payload, 0);
        assert_ne!(buf.len(), 0);
        let len = min(buf.len(), *remaining_object_payload);
        let n = match self.buf.chain_read2(|b| t.recv_stream(self.stream_id, b), &mut buf[..len]) {
            Ok(v) => v,
            Err(StreamError::Done) => return Err(Error::Done),
            Err(StreamError::Fin)
            | Err(StreamError::StreamReset(_))
            | Err(StreamError::InvalidStreamState(_)) => return Err(Error::Fin),
            Err(e) => return Err(Error::Transport(e)),
        };
        #[cfg(feature = "qlog")]
        let State::Subgroup { remaining_object_payload, subgroup_header, current_object_id } =
            &mut self.state
        else {
            unreachable!()
        };
        #[cfg(not(feature = "qlog"))]
        let State::Subgroup { remaining_object_payload, .. } = &mut self.state else {
            unreachable!()
        };
        *remaining_object_payload -= n;
        #[cfg(feature = "qlog")]
        if *remaining_object_payload == 0 {
            if let Some(qlog) = t.qlog_streamer() {
                let sh = subgroup_header.as_ref().unwrap();
                qlog.add_event_now(qlog::events::JsonEvent {
                    time: 0.0,
                    importance: qlog::events::EventImportance::Core,
                    name: "moqt:subgroup_object_received".into(),
                    data: serde_json::json!({
                        "stream_id": self.stream_id,
                        "group_id": sh.group_id(),
                        "subgroup_id": sh.subgroup_id(),
                        "object_id": current_object_id.unwrap(),
                    }),
                }).ok();
            }
        }
        Ok(n)
    }
}

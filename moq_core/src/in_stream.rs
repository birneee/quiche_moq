use crate::error::{Error, Result};
use crate::transport::{StreamError, Transport};
use log::debug;
use crate::prelude::*;
use moq_wire::object::ObjectHeader;
use moq_wire::subgroup::SubgroupHeader;
use moq_wire::{FromBytes, RequestId, Version, PADDING_STREAM_TYPE_ID};
use moq_wire::fetch::{FetchEntry, FetchStream, FETCH_HEADER_TYPE};
use short_buf::ShortBuf;
use core::cmp::min;

// Must be large enough to hold the complete serialised object header in one fill.
// LOC VideoConfig alone can be ~200 bytes; 4096 gives headroom.
const BUF_LEN: usize = 4096;

// Matches the chunk size `OutPaddingStream` sends per call.
const PADDING_DRAIN_BUF_LEN: usize = 1 << 16;

/// A subgroup stream after its header.
struct SubgroupState {
    header: SubgroupHeader,
    remaining_object_payload: usize,
    current_object_id: Option<u64>,
    /// Subgroup ID for headers that encode it as the first Object ID.
    first_object_id: Option<u64>,
}

impl SubgroupState {
    fn subgroup_location(&self) -> Option<moq_wire::SubgroupLocation> {
        let subgroup_id = self.header.subgroup_id().or(self.first_object_id)?;
        Some(moq_wire::SubgroupLocation { group_id: self.header.group_id(), subgroup_id })
    }
}

/// An object header read from a subgroup stream, with where the object is.
pub struct ObjectRead {
    pub location: moq_wire::Location,
    pub subgroup_id: u64,
    /// the Subgroup header's
    pub publisher_priority: u8,
    /// The Subgroup holds the Group's largest Object (draft-ietf-moq-transport-18 §11.4.2).
    pub end_of_group: bool,
    pub header: ObjectHeader,
}

enum State {
    /// Stream type has not been read yet.
    Type,
    /// Subgroup header not yet read.
    SubgroupHeader,
    /// Ready subgroup stream.
    Subgroup(SubgroupState),
    /// Padding stream — data is discarded.
    Padding,
    /// A fetch stream before its Request ID.
    FetchHeader,
    /// A fetch stream, its codec attached by `start_fetch` once the session knows the fetch.
    Fetch(FetchState),
}

/// A fetch stream after its FETCH_HEADER.
struct FetchState {
    request_id: RequestId,
    codec: Option<FetchStream>,
    remaining_object_payload: usize,
}

/// Outcome of [`InStream::handle`].
pub enum Handled<'a> {
    /// This is a padding stream; one chunk was just discarded.
    Padding,
    /// The subgroup header is available.
    Subgroup(&'a SubgroupHeader),
    /// A fetch stream answering our FETCH `RequestId`, see `start_fetch`.
    Fetch(RequestId),
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

    /// A stream whose `first_byte` was already read, e.g. telling it from the control stream.
    pub(crate) fn with_first_byte(stream_id: u64, version: Version, first_byte: u8) -> Self {
        let mut stream = Self::new(stream_id, version);
        let _ = stream.buf.fill(|b| {
            b[0] = first_byte;
            Ok::<_, ()>(1)
        });
        stream
    }

    /// Drives this stream's state machine.
    /// - [`Error::Done`]: no progress possible right now
    /// - [`Error::Fin`] / [`StreamError::StreamReset`]: the stream is done
    /// - [`Error::Wire`]: malformed stream header
    pub fn handle<T: Transport>(&mut self, t: &mut T) -> Result<Handled<'_>> {
        loop {
            match self.state {
                State::Type => self.poll_type(t)?,
                State::Padding => {
                    self.drain_padding(t)?;
                    return Ok(Handled::Padding);
                }
                State::SubgroupHeader => self.poll_subgroup_header(t)?,
                State::FetchHeader => self.poll_fetch_header(t)?,
                State::Fetch(ref fetch) => return Ok(Handled::Fetch(fetch.request_id)),
                State::Subgroup(_) => break,
            }
        }
        let State::Subgroup(subgroup) = &self.state else { unreachable!("the loop ends in this state") };
        Ok(Handled::Subgroup(&subgroup.header))
    }

    fn poll_type<T: Transport>(&mut self, t: &mut T) -> Result<()> {
        loop {
            let mut b = self.version.reader(self.buf.buffer());
            match b.varint() {
                Ok(ty) => {
                    if ty == PADDING_STREAM_TYPE_ID {
                        debug!("stream {} is a padding stream", self.stream_id);
                        self.buf.consume(b.consumed());
                        self.state = State::Padding;
                    } else if ty == FETCH_HEADER_TYPE {
                        self.buf.consume(b.consumed());
                        self.state = State::FetchHeader;
                    } else {
                        // Leave type bytes unconsumed; SubgroupHeader::from_bytes reads the type.
                        self.state = State::SubgroupHeader;
                    }
                    return Ok(());
                }
                Err(moq_wire::DecodeError::Incomplete { .. }) => {
                    fill_header_buf(&mut self.buf, self.stream_id, t)?;
                }
                Err(e) => return Err(moq_wire::Error::from(e).into()),
            }
        }
    }

    fn poll_fetch_header<T: Transport>(&mut self, t: &mut T) -> Result<()> {
        loop {
            let mut b = self.version.reader(self.buf.buffer());
            match FetchStream::decode_header(&mut b) {
                Ok(request_id) => {
                    self.buf.consume(b.consumed());
                    self.state = State::Fetch(FetchState { request_id, codec: None, remaining_object_payload: 0 });
                    return Ok(());
                }
                Err(moq_wire::Error::Incomplete { .. }) => fill_header_buf(&mut self.buf, self.stream_id, t)?,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Attaches the codec of the fetch this stream answers.
    pub(crate) fn start_fetch(&mut self, codec: FetchStream) {
        if let State::Fetch(fetch) = &mut self.state {
            fetch.codec = Some(codec);
        }
    }

    /// The next entry of a fetch stream once `start_fetch` attached its codec, its payload read with `read_obj_pld`.
    pub(crate) fn read_fetch_entry<T: Transport>(&mut self, t: &mut T) -> Result<FetchEntry> {
        let Self { stream_id, version, state, buf } = self;
        let State::Fetch(FetchState { codec: Some(codec), remaining_object_payload, .. }) = state else { return Err(Error::Done) };
        if *remaining_object_payload > 0 {
            return Err(Error::UnfinishedPayload);
        }
        loop {
            let mut r = version.reader(buf.buffer());
            match codec.decode(&mut r) {
                Ok(entry) => {
                    buf.consume(r.consumed());
                    if let FetchEntry::Object(object) = &entry {
                        *remaining_object_payload = object.payload_len;
                    }
                    return Ok(entry);
                }
                Err(moq_wire::Error::Incomplete { .. }) => fill_header_buf(buf, *stream_id, t)?,
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn drain_padding<T: Transport>(&mut self, t: &mut T) -> Result<()> {
        debug_assert!(matches!(self.state, State::Padding));
        let mut scratch = [0u8; PADDING_DRAIN_BUF_LEN];
        loop {
            match self.buf.chain_read2(|b| t.recv_stream(self.stream_id, b), &mut scratch) {
                Ok(_) => {}
                Err(StreamError::Done) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn poll_subgroup_header<T: Transport>(&mut self, t: &mut T) -> Result<()> {
        let h = loop {
            let mut b = self.version.reader(self.buf.buffer());
            match SubgroupHeader::from_bytes(&mut b, self.version) {
                Ok(h) => {
                    self.buf.consume(b.consumed());
                    break h;
                }
                Err(moq_wire::Error::Incomplete { .. }) => fill_header_buf(&mut self.buf, self.stream_id, t)?,
                Err(e) => return Err(e.into()),
            }
        };

        debug!("parsed subgroup header: {:?}", h);
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::default(),
                name: "moqt:subgroup_header_parsed".into(),
                data: serde_json::json!({
                    "stream_id": self.stream_id,
                    "track_alias": h.track_alias().0,
                    "group_id": h.group_id(),
                    "subgroup_id": h.subgroup_id(),
                    "publisher_priority": h.publisher_priority(),
                }),
            }).ok();
        }
        self.state = State::Subgroup(SubgroupState { header: h, remaining_object_payload: 0, current_object_id: None, first_object_id: None });
        Ok(())
    }

    fn subgroup(&self) -> Option<&SubgroupState> {
        match &self.state {
            State::Subgroup(subgroup) => Some(subgroup),
            _ => None,
        }
    }

    /// [`Error::Done`] before the subgroup header, [`Error::UnfinishedPayload`] with payload left.
    fn read_next_object_header<T: Transport>(&mut self, t: &mut T) -> Result<ObjectRead> {
        let State::Subgroup(subgroup) = &mut self.state else { return Err(Error::Done) };
        if subgroup.remaining_object_payload > 0 {
            return Err(Error::UnfinishedPayload);
        }
        let object_header = loop {
            let mut b = self.version.reader(self.buf.buffer());
            match ObjectHeader::decode(&mut b, self.version, &subgroup.header, subgroup.current_object_id) {
                Ok(header) => {
                    self.buf.consume(b.consumed());
                    break header;
                }
                Err(moq_wire::Error::Incomplete { .. }) => fill_header_buf(&mut self.buf, self.stream_id, t)?,
                Err(e) => return Err(e.into()),
            }
        };
        debug!("parsed object header: {:?}", object_header);
        let object_id = object_header.id();
        subgroup.remaining_object_payload = object_header.payload_len();
        subgroup.current_object_id = Some(object_id);
        let first_object_id = *subgroup.first_object_id.get_or_insert(object_id);
        Ok(ObjectRead {
            location: moq_wire::Location { group: subgroup.header.group_id(), object: object_id },
            subgroup_id: subgroup.header.subgroup_id().unwrap_or(first_object_id),
            publisher_priority: subgroup.header.publisher_priority(),
            end_of_group: subgroup.header.end_of_group(),
            header: object_header,
        })
    }

    pub fn read_obj_hdr<T: Transport>(&mut self, t: &mut T) -> Result<ObjectRead> {
        let read = self.read_next_object_header(t)?;
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer()
            && let Some(subgroup) = self.subgroup()
        {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::default(),
                name: "moqt:subgroup_object_parsed".into(),
                data: serde_json::json!({
                    "stream_id": self.stream_id,
                    "group_id": subgroup.header.group_id(),
                    "subgroup_id": subgroup.header.subgroup_id(),
                    "object_id": read.header.id(),
                    "extension_headers_length": read.header.properties_len() as u64,
                    "extension_headers": read.header.properties_to_qlog(),
                    "object_payload_length": read.header.payload_len() as u64,
                    "object_status": read.header.status().map(moq_wire::object::ObjectStatus::encode),
                }),
            }).ok();
        }
        Ok(read)
    }

    /// 0 if the next object header can be read.
    pub fn remaining_object_payload(&self) -> usize {
        match &self.state {
            State::Subgroup(subgroup) => subgroup.remaining_object_payload,
            State::Fetch(fetch) => fetch.remaining_object_payload,
            _ => 0,
        }
    }

    pub fn subgroup_header(&self) -> Option<&SubgroupHeader> {
        self.subgroup().map(|subgroup| &subgroup.header)
    }

    /// The object whose header was read last, `None` before the first.
    pub fn current_location(&self) -> Option<moq_wire::Location> {
        let subgroup = self.subgroup()?;
        Some(moq_wire::Location { group: subgroup.header.group_id(), object: subgroup.current_object_id? })
    }

    /// The stream's subgroup, once its header is parsed. Fixed for the stream.
    pub fn subgroup_location(&self) -> Option<moq_wire::SubgroupLocation> {
        self.subgroup()?.subgroup_location()
    }

    /// Reads and drops up to `n` payload bytes, as many as are available now.
    pub(crate) fn discard_obj_pld<T: Transport>(&mut self, t: &mut T, n: usize) -> Result<usize> {
        let mut scratch = [0u8; 4096];
        let mut discarded = 0;
        while discarded < n {
            let want = (n - discarded).min(scratch.len());
            match self.read_obj_pld(t, &mut scratch[..want]) {
                Ok(0) | Err(Error::Done) => break,
                Ok(read) => discarded += read,
                Err(e) => return Err(e),
            }
        }
        Ok(discarded)
    }

    /// Returns [`Error::Done`] when no payload is available right now.
    pub fn read_obj_pld<T: Transport>(&mut self, t: &mut T, buf: &mut [u8]) -> Result<usize> {
        let remaining = match &mut self.state {
            State::Subgroup(subgroup) => &mut subgroup.remaining_object_payload,
            State::Fetch(fetch) => &mut fetch.remaining_object_payload,
            _ => return Err(Error::Done),
        };
        if *remaining == 0 {
            return Err(Error::Done);
        }
        let len = min(buf.len(), *remaining);
        let n = match self.buf.chain_read2(|b| t.recv_stream(self.stream_id, b), &mut buf[..len]) {
            Ok(n) => n,
            Err(StreamError::Done) => return Err(Error::Done),
            Err(StreamError::Fin | StreamError::StreamReset(_) | StreamError::InvalidStreamState(_)) => return Err(Error::Fin),
            Err(e) => return Err(Error::Transport(e)),
        };
        *remaining -= n;
        #[cfg(feature = "qlog")]
        if let State::Subgroup(subgroup) = &self.state
            && subgroup.remaining_object_payload == 0
            && let Some(qlog) = t.qlog_streamer()
        {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::default(),
                name: "moqt:subgroup_object_received".into(),
                data: serde_json::json!({
                    "stream_id": self.stream_id,
                    "group_id": subgroup.header.group_id(),
                    "subgroup_id": subgroup.header.subgroup_id(),
                    "object_id": subgroup.current_object_id,
                }),
            }).ok();
        }
        Ok(n)
    }
}

/// Reads more of a stream or object header.
/// - a FIN before it is complete ends the stream, nothing can follow
/// - a header that doesn't fit the buffer is malformed
fn fill_header_buf(buf: &mut ShortBuf<BUF_LEN>, stream_id: u64, t: &mut impl Transport) -> Result<()> {
    if buf.len() == BUF_LEN {
        return Err(moq_wire::Error::ProtocolViolation(format!("header longer than {BUF_LEN} bytes")).into());
    }
    match buf.fill(|b| t.recv_stream(stream_id, b)) {
        Ok(_) => Ok(()),
        Err(StreamError::Done) => Err(Error::Done),
        Err(StreamError::Fin | StreamError::InvalidStreamState(_)) => Err(Error::Fin),
        Err(e) => Err(e.into()),
    }
}

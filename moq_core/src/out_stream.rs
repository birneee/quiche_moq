use crate::error::Result;
use crate::transport::{StreamError, Transport};
use crate::Error;
use log::{trace, warn};
use octets::OctetsMut;
use moq_wire::{
    KeyValuePairs, RESET_STREAM_CODE_CANCELED, SubgroupType, ToBytes, TrackAlias, Version,
};
use moq_wire::object::ObjectHeader;
use moq_wire::subgroup::SubgroupHeader;

enum State {
    SubgroupHeader,
    ObjectHeader { subgroup_ty: SubgroupType },
    ObjectPayload { subgroup_ty: SubgroupType, remaining_bytes: usize },
}

/// Manages one outgoing subgroup stream.
pub(crate) struct OutStream {
    stream_id: u64,
    state: State,
    track_alias: TrackAlias,
    group_id: u64,
    subgroup_id: u64,
    version: Version,
    next_object_id: u64,
}

impl OutStream {
    pub fn new(
        stream_id: u64,
        track_alias: TrackAlias,
        group_id: u64,
        subgroup_id: u64,
        version: Version,
    ) -> Self {
        Self {
            stream_id,
            state: State::SubgroupHeader,
            track_alias,
            group_id,
            subgroup_id,
            version,
            next_object_id: 0,
        }
    }

    /// `object_id`: `None` to auto-increment; `Some(id)` for explicit id (must be >= next expected).
    ///
    /// # Errors
    /// - [`Error::UnfinishedPayload`]: called while the previous object's payload is in progress.
    /// - [`Error::InsufficientCapacity`]: QUIC stream capacity exhausted; retry later.
    pub fn send_obj_hdr<T: Transport>(
        &mut self,
        object_id: Option<u64>,
        size: usize,
        extension_headers: &KeyValuePairs,
        t: &mut T,
    ) -> Result<()> {
        assert!(size > 0);
        loop {
            match self.state {
                State::SubgroupHeader => {
                    let subgroup =
                        SubgroupHeader::new(self.track_alias, self.group_id, self.subgroup_id, self.version);
                    let mut b = [0u8; 1024];
                    let mut o = OctetsMut::with_slice(&mut b);
                    subgroup.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    match t.send_stream_if_capacity(self.stream_id, &b[..len], false) {
                        Ok(_) => {}
                        Err(StreamError::InsufficientCapacity) => {
                            return Err(Error::InsufficientCapacity)
                        }
                        Err(e) => return Err(e.into()),
                    }
                    trace!("sent subgroup header on stream {}", self.stream_id);
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = t.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::Core,
                            name: "moqt:subgroup_header_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id,
                                "track_alias": subgroup.track_alias(),
                                "group_id": subgroup.group_id(),
                                "subgroup_id": subgroup.subgroup_id(),
                                "publisher_priority": subgroup.publisher_priority(),
                            }),
                        }).ok();
                    }
                    self.state = State::ObjectHeader { subgroup_ty: subgroup.ty() };
                    continue;
                }
                State::ObjectHeader { subgroup_ty } => {
                    let object_id = match object_id {
                        None => self.next_object_id,
                        Some(id) => {
                            assert!(
                                id >= self.next_object_id,
                                "object_id {id} < next expected {}",
                                self.next_object_id
                            );
                            id
                        }
                    };
                    let object_header =
                        ObjectHeader::new(object_id, size, subgroup_ty, extension_headers.clone());
                    let mut b = [0u8; 1024];
                    let mut o = OctetsMut::with_slice(&mut b);
                    object_header.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    match t.send_stream_if_capacity(self.stream_id, &b[..len], false) {
                        Ok(_) => {}
                        Err(StreamError::InsufficientCapacity) => {
                            return Err(Error::InsufficientCapacity)
                        }
                        Err(e) => return Err(e.into()),
                    }
                    self.next_object_id = object_id + 1;
                    trace!("sent {:?} on stream {}", object_header, self.stream_id);
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = t.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::Core,
                            name: "moqt:subgroup_object_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id,
                                "object_id": object_header.id(),
                                "extension_headers_length": object_header.extension_headers_len() as u64,
                                "extension_headers": object_header.extension_headers_to_qlog(),
                                "object_payload_length": object_header.payload_len() as u64,
                                "object_status": object_header.status(),
                            }),
                        }).ok();
                    }
                    self.state = State::ObjectPayload {
                        subgroup_ty,
                        remaining_bytes: size,
                    };
                    return Ok(());
                }
                State::ObjectPayload { .. } => return Err(Error::UnfinishedPayload),
            }
        }
    }

    /// # Errors
    /// - [`Error::ExceededPayload`]: `buf` longer than the remaining object payload.
    /// - [`Error::Done`]: send buffer full; retry with the same data.
    /// - [`Error::InsufficientCapacity`]: QUIC stream capacity exhausted; retry later.
    pub fn send_obj_pld<T: Transport>(&mut self, buf: &[u8], t: &mut T) -> Result<usize> {
        match &mut self.state {
            State::SubgroupHeader | State::ObjectHeader { .. } => {
                panic!("no object header sent")
            }
            State::ObjectPayload { remaining_bytes, subgroup_ty } => {
                if *remaining_bytes < buf.len() {
                    return Err(Error::ExceededPayload);
                }
                let n = match t.send_stream(self.stream_id, buf, false) {
                    Ok(v) => v,
                    Err(StreamError::Done) => return Err(Error::Done),
                    Err(StreamError::InvalidStreamState(_)) => return Err(Error::Done),
                    Err(StreamError::InsufficientCapacity) => return Err(Error::InsufficientCapacity),
                    Err(e) => return Err(e.into()),
                };
                *remaining_bytes -= n;
                trace!(
                    "sent {} byte object payload on stream {}, {} bytes remaining",
                    n,
                    self.stream_id,
                    *remaining_bytes
                );
                if *remaining_bytes == 0 {
                    self.state = State::ObjectHeader { subgroup_ty: *subgroup_ty };
                }
                Ok(n)
            }
        }
    }

    pub fn group_id(&self) -> u64 {
        self.group_id
    }

    pub fn subgroup_id(&self) -> u64 {
        self.subgroup_id
    }

    /// Close this subgroup stream with a QUIC FIN.
    ///
    /// If a payload is still in progress the stream is reset instead so the peer
    /// discards the incomplete object cleanly.
    pub fn fin<T: Transport>(&mut self, t: &mut T) {
        if matches!(self.state, State::ObjectPayload { .. }) {
            warn!("group stream switched with in-progress payload — resetting instead of FIN");
            t.reset_stream(self.stream_id, RESET_STREAM_CODE_CANCELED);
        } else {
            t.send_stream(self.stream_id, &[], true).ok();
        }
    }

    /// Send an entire object atomically.
    pub fn send_obj<T: Transport>(&mut self, t: &mut T, buf: &[u8]) -> Result<()> {
        self.send_obj_hdr(None, buf.len(), &KeyValuePairs::new(), t)?;
        let n = self.send_obj_pld(buf, t).unwrap();
        assert_eq!(n, buf.len());
        Ok(())
    }
}

use crate::prelude::*;
use crate::error::Result;
use crate::transport::{StreamError, Transport};
use crate::Error;
use log::{trace, warn};
use moq_wire::{KeyValuePairs, ResetCode, ToBytes, TrackAlias, Version};
use moq_wire::subgroup::SubgroupType;
use moq_wire::object::{ObjectHeader, ObjectStatus};
use moq_wire::subgroup::SubgroupHeader;
use crate::time::Timestamp;
use core::time::Duration;

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
    publisher_priority: u8,
    version: Version,
    /// Whether this subgroup contains the largest Object in the Group —
    /// folded into the SUBGROUP_HEADER's END_OF_GROUP bit (draft-14+). The
    /// caller decides this; `OutStream` just carries it through to the wire.
    end_of_group: bool,
    /// the previous object's ID, Object IDs are deltas to it from draft 14
    last_object_id: Option<u64>,
    /// When the object whose payload is in progress was received, if given.
    received_at: Option<Timestamp>,
    /// When the FIN was written, the start of the timer until it is acked.
    fin_sent_at: Option<Timestamp>,
    /// reused for every header on this stream
    header_buf: Vec<u8>,
}

impl OutStream {
    pub fn new(
        stream_id: u64,
        track_alias: TrackAlias,
        group_id: u64,
        subgroup_id: u64,
        publisher_priority: u8,
        version: Version,
        end_of_group: bool,
    ) -> Self {
        Self {
            stream_id,
            state: State::SubgroupHeader,
            track_alias,
            group_id,
            subgroup_id,
            publisher_priority,
            version,
            end_of_group,
            last_object_id: None,
            received_at: None,
            fin_sent_at: None,
            header_buf: Vec::new(),
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
        status: ObjectStatus,
        properties: &KeyValuePairs,
        received_at: Option<Timestamp>,
        t: &mut T,
    ) -> Result<()> {
        loop {
            match self.state {
                State::SubgroupHeader => {
                    let subgroup =
                        SubgroupHeader::new(self.track_alias, self.group_id, self.subgroup_id, self.version)
                            .with_publisher_priority(self.publisher_priority)
                            .with_end_of_group(self.end_of_group);
                    self.header_buf.clear();
                    subgroup.to_bytes(&mut self.version.writer(&mut self.header_buf), self.version)?;
                    match t.send_stream_if_capacity(self.stream_id, &self.header_buf, false) {
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
                            importance: qlog::events::EventImportance::default(),
                            name: "moqt:subgroup_header_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id,
                                "track_alias": subgroup.track_alias().0,
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
                        None => self.next_object_id(),
                        Some(id) => {
                            assert!(id >= self.next_object_id(), "object_id {id} < next expected {}", self.next_object_id());
                            id
                        }
                    };
                    let object_header =
                        ObjectHeader::new(object_id, size, subgroup_ty, properties.clone()).with_status(status);
                    self.header_buf.clear();
                    object_header.encode(&mut self.version.writer(&mut self.header_buf), self.version, self.last_object_id)?;
                    match t.send_stream_if_capacity(self.stream_id, &self.header_buf, false) {
                        Ok(_) => {}
                        Err(StreamError::InsufficientCapacity) => {
                            return Err(Error::InsufficientCapacity)
                        }
                        Err(e) => return Err(e.into()),
                    }
                    self.last_object_id = Some(object_id);
                    trace!("sent {:?} on stream {}", object_header, self.stream_id);
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = t.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::default(),
                            name: "moqt:subgroup_object_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id,
                                "object_id": object_header.id(),
                                "extension_headers_length": object_header.properties_len() as u64,
                                "extension_headers": object_header.properties_to_qlog(),
                                "object_payload_length": object_header.payload_len() as u64,
                                "object_status": object_header.status().map(ObjectStatus::encode),
                            }),
                        }).ok();
                    }
                    // a zero-length object has no payload to wait for
                    if size == 0 {
                        self.state = State::ObjectHeader { subgroup_ty };
                        self.received_at = None;
                    } else {
                        self.state = State::ObjectPayload { subgroup_ty, remaining_bytes: size };
                        self.received_at = received_at;
                    }
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
                    self.received_at = None;
                }
                Ok(n)
            }
        }
    }

    /// When this stream expires, `None` while nothing is pending: the object being written `object_timeout` after
    /// it was received, once FIN'd by `fin_timeout` after the FIN until it is acked (draft-ietf-moq-transport-19 §8).
    pub fn deadline(&self, object_timeout: Option<Duration>, fin_timeout: Option<Duration>) -> Option<Timestamp> {
        let object = self.received_at.zip(object_timeout).map(|(at, timeout)| at + timeout);
        let fin = self.fin_sent_at.zip(fin_timeout).map(|(at, timeout)| at + timeout);
        object.into_iter().chain(fin).min()
    }

    pub fn fin_sent(&self) -> bool {
        self.fin_sent_at.is_some()
    }

    /// Payload bytes not yet sent for the current `ObjectPayload` object, if
    /// any. `None` once fully sent (state moves back to `ObjectHeader` the same tick).
    pub fn remaining_payload(&self) -> Option<usize> {
        match self.state {
            State::ObjectPayload { remaining_bytes, .. } => Some(remaining_bytes),
            _ => None,
        }
    }

    /// One past the last object id sent, 0 before the first.
    pub fn next_object_id(&self) -> u64 {
        self.last_object_id.map_or(0, |id| id + 1)
    }

    /// The object_id currently mid-payload on this stream, if any — for
    /// `send_obj_pld`'s lookup.
    pub fn current_object_id(&self) -> Option<u64> {
        match self.state {
            State::ObjectPayload { .. } => self.last_object_id,
            _ => None,
        }
    }

    /// Close this subgroup stream with a QUIC FIN.
    ///
    /// If a payload is still in progress the stream is reset instead (draft-16
    /// §10.4.3), returns `true` then: the stream is done, the subgroup may
    /// continue on a new one.
    pub fn fin<T: Transport>(&mut self, t: &mut T, now: Timestamp) -> bool {
        if matches!(self.state, State::ObjectPayload { .. }) {
            warn!("subgroup ended with in-progress payload — resetting instead of FIN");
            t.reset_stream(self.stream_id, ResetCode::Canceled);
            return true;
        }
        if self.fin_sent_at.is_none() {
            t.send_stream(self.stream_id, &[], true).ok();
            self.fin_sent_at = Some(now);
        }
        false
    }

}

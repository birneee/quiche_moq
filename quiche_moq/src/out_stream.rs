use crate::error::Result;
use crate::Error;
use log::trace;
use octets::OctetsMut;
use quiche_moq_wire::{KeyValuePairs, SubgroupType, ToBytes, TrackAlias, Version};
use quiche_moq_wire::object::ObjectHeader;
use quiche_moq_wire::subgroup::SubgroupHeader;
use quiche_utils::stream_id::StreamID;
use quiche_webtransport as wt;

enum State {
    SubgroupHeader,
    ObjectHeader { subgroup_ty: SubgroupType },
    ObjectPayload { subgroup_ty: SubgroupType, remaining_bytes: usize },
}

/// Manages one subgroup stream
pub(crate) struct OutStream {
    stream_id: StreamID,
    state: State,
    track_alias: TrackAlias,
    group_id: u64,
    subgroup_id: u64,
    version: Version,
    next_object_id: u64,
}

impl OutStream {
    pub fn new(
        stream_id: StreamID,
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

    /// `object_id`: `None` to auto-increment; `Some(id)` to use explicit id (must be >= next expected).
    ///
    /// # Errors
    /// - [`Error::UnfinishedPayload`]: called while the previous object's payload is still in progress.
    /// - [`Error::InsufficientCapacity`]: QUIC stream capacity exhausted; retry later.
    pub fn send_obj_hdr(
        &mut self,
        object_id: Option<u64>,
        size: usize,
        extension_headers: &KeyValuePairs,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
    ) -> Result<()> {
        assert!(size > 0);
        loop {
            match self.state {
                State::SubgroupHeader => {
                    let subgroup = SubgroupHeader::new(self.track_alias, self.group_id, self.subgroup_id, self.version);
                    let mut b = [0u8; 100];
                    let mut o = OctetsMut::with_slice(&mut b);
                    subgroup.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    match wt.stream_send_if_capacity(self.stream_id.into(), quic, &b[..len], false) {
                        Ok(_) => {}
                        Err(wt::Error::InsufficientCapacity) => return Err(Error::InsufficientCapacity),
                        Err(e) => unimplemented!("{:?}", e),
                    }
                    trace!("sent subgroup header on stream {}", self.stream_id);
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = quic.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::Core,
                            name: "moqt:subgroup_header_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id.into_u64(),
                                "track_alias": subgroup.track_alias(),
                                "group_id": subgroup.group_id(),
                                "subgroup_id": subgroup.subgroup_id(),
                                "publisher_priority": subgroup.publisher_priority(),
                            }),
                        }).ok();
                    }
                    self.state = State::ObjectHeader {
                        subgroup_ty: subgroup.ty()
                    };
                    continue;
                }
                State::ObjectHeader { subgroup_ty } => {
                    let object_id = match object_id {
                        None => {
                            let id = self.next_object_id;
                            self.next_object_id += 1;
                            id
                        }
                        Some(id) => {
                            assert!(id >= self.next_object_id, "object_id {id} < next expected {}", self.next_object_id);
                            self.next_object_id = id + 1;
                            id
                        }
                    };
                    let object_header = ObjectHeader::new(object_id, size, subgroup_ty, extension_headers.clone());
                    let mut b = [0u8; 100];
                    let mut o = OctetsMut::with_slice(&mut b);
                    object_header.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    match wt.stream_send_if_capacity(self.stream_id.into(), quic, &b[..len], false) {
                        Ok(_) => {}
                        Err(wt::Error::InsufficientCapacity) => return Err(Error::InsufficientCapacity),
                        Err(e) => unimplemented!("{:?}", e),
                    }
                    trace!("sent {:?} on stream {}", object_header, self.stream_id);
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = quic.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::Core,
                            name: "moqt:subgroup_object_created".into(),
                            data: serde_json::json!({
                                "stream_id": self.stream_id.into_u64(),
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
                State::ObjectPayload { .. } => {
                    return Err(Error::UnfinishedPayload);
                }
            }
        }
    }

    /// # Errors
    /// - [`Error::ExceededPayload`]: `buf` is longer than the remaining object payload.
    /// - [`Error::Done`]: send buffer full; retry with the same data.
    /// - [`Error::InsufficientCapacity`]: QUIC stream capacity exhausted; retry later.
    pub fn send_obj_pld(
        &mut self,
        buf: &[u8],
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        match &mut self.state {
            State::SubgroupHeader | State::ObjectHeader { .. } => {
                panic!("no object header sent")
            }
            State::ObjectPayload { remaining_bytes, subgroup_ty } => {
                if *remaining_bytes < buf.len() {
                    return Err(Error::ExceededPayload);
                }
                let n = match wt.stream_send(self.stream_id.into(), quic, buf, false) {
                    Ok(v) => v,
                    Err(wt::Error::Done) => return Err(Error::Done),
                    Err(wt::Error::InvalidStreamState(_)) => return Err(Error::Done),
                    Err(wt::Error::InsufficientCapacity) => return Err(Error::InsufficientCapacity),
                    Err(e) => unimplemented!("{:?}", e),
                };
                *remaining_bytes -= n;
                trace!(
                    "sent {} byte object payload on stream {}, {} bytes remaining",
                    n, self.stream_id, *remaining_bytes
                );
                if *remaining_bytes == 0 {
                    self.state = State::ObjectHeader {
                        subgroup_ty: *subgroup_ty,
                    };
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

    /// Send a QUIC FIN to close this subgroup stream.
    /// Must only be called between objects (not while an object payload is in progress).
    pub fn fin(&mut self, wt: &mut wt::Connection, quic: &mut quiche::Connection) {
        assert!(
            !matches!(self.state, State::ObjectPayload { .. }),
            "cannot fin stream while object payload is in progress"
        );
        wt.stream_send(self.stream_id.into(), quic, &[], true).ok();
    }

    /// do not send partially
    pub fn send_obj(
        &mut self,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        buf: &[u8],
    ) -> Result<()> {
        self.send_obj_hdr(None, buf.len(), &KeyValuePairs::new(), quic, wt)?;
        let n = self.send_obj_pld(buf, wt, quic).unwrap();
        assert_eq!(n, buf.len());
        Ok(())
    }
}

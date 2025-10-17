use crate::error::Result;
use crate::Error;
use log::trace;
use octets::OctetsMut;
use quiche_moq_wire::{SubgroupType, ToBytes, TrackAlias, Version};
use quiche_moq_wire::object::ObjectHeader;
use quiche_moq_wire::subgroup::SubgroupHeader;
use quiche_utils::stream_id::StreamID;
use quiche_webtransport as wt;

enum State {
    SubgroupHeader,
    ObjectHeader { subgroup_ty: SubgroupType },
    ObjectPayload { subgroup_ty: SubgroupType, remaining_bytes: usize },
}

pub(crate) struct OutStream {
    stream_id: StreamID,
    state: State,
    track_alias: TrackAlias,
    version: Version,
}

impl OutStream {
    pub fn new(
        stream_id: StreamID,
        track_alias: TrackAlias,
        version: Version,
    ) -> Self {
        Self {
            stream_id,
            state: State::SubgroupHeader,
            track_alias,
            version,
        }
    }

    pub fn send_obj_hdr(
        &mut self,
        size: usize,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
    ) -> Result<()> {
        assert!(size > 0);
        loop {
            match self.state {
                State::SubgroupHeader => {
                    let subgroup = SubgroupHeader::new(self.track_alias, 0, 0, self.version);
                    let mut b = [0u8; 100];
                    let mut o = OctetsMut::with_slice(&mut b);
                    subgroup.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    wt.stream_send_if_capacity(self.stream_id.into(), quic, &b[..len], false)
                        .unwrap();
                    trace!("sent subgroup header on stream {}", self.stream_id);
                    self.state = State::ObjectHeader {
                        subgroup_ty: subgroup.ty()
                    };
                    continue;
                }
                State::ObjectHeader { subgroup_ty } => {
                    let object_header = ObjectHeader::new(0, size, subgroup_ty);
                    let mut b = [0u8; 100];
                    let mut o = OctetsMut::with_slice(&mut b);
                    object_header.to_bytes(&mut o, self.version)?;
                    let len = o.off();
                    wt.stream_send_if_capacity(self.stream_id.into(), quic, &b[..len], false)
                        .unwrap();
                    trace!("sent {:?} on stream {}", object_header, self.stream_id);
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
                assert!(*remaining_bytes >= buf.len());
                let n = match wt.stream_send(self.stream_id.into(), quic, buf, false) {
                    Ok(v) => v,
                    Err(wt::Error::Done) => return Err(Error::Done),
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

    /// do not send partially
    pub fn send_obj(
        &mut self,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        buf: &[u8],
    ) -> Result<()> {
        self.send_obj_hdr(buf.len(), quic, wt)?;
        let n = self.send_obj_pld(&buf, wt, quic).unwrap();
        assert_eq!(n, buf.len());
        Ok(())
    }
}

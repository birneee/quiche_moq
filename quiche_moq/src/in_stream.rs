use crate::error::{Error, Result};
use log::debug;
use octets::Octets;
use quiche::h3;
use short_buf::ShortBuf;
use std::cmp::min;
use quiche_moq_wire::object::ObjectHeader;
use quiche_moq_wire::subgroup::SubgroupHeader;
use quiche_moq_wire::{FromBytes, Version};
use quiche_utils::stream_id::StreamID;

const BUF_LEN: usize = 100;

pub struct InStream {
    stream_id: StreamID,
    session_id: StreamID,
    version: Version,
    subgroup_header: Option<SubgroupHeader>,
    remaining_object_payload: usize,
    readable: bool,
    /// buffer used to temporary store subgroup and object header.
    /// And maybe also short object payloads.
    buf: ShortBuf<BUF_LEN>,
    /// WebTransport reported fin
    wt_fin: bool,
}

impl InStream {
    pub fn new(stream_id: StreamID, session_id: StreamID, version: Version) -> Self {
        Self {
            stream_id,
            session_id,
            version,
            subgroup_header: None,
            remaining_object_payload: 0,
            readable: false,
            buf: ShortBuf::new(),
            wt_fin: false,
        }
    }

    pub fn mark_readable(&mut self) {
        self.readable = true;
    }

    /// This is called when wt stream is readable
    pub fn read(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) -> quiche_moq_wire::Result<()> {
        if self.buf.is_empty() {
            match self.buf.fill(|b| {
                wt.recv_stream(self.stream_id.into(), self.session_id.into(), h3, quic, b)
            }) {
                Ok(_) => {}
                Err(quiche_webtransport::Error::Done) => {
                    self.readable = false;
                    return Ok(());
                }
                Err(quiche_webtransport::Error::Fin) => {
                    self.readable = false;
                    self.wt_fin = true;
                    return Ok(());
                }
                Err(e) => unimplemented!("{:?}", e),
            }
        }

        if self.subgroup_header.is_none() {
            let _ty = Octets::with_slice(self.buf.buffer()).get_varint()?;
            let mut b = Octets::with_slice(self.buf.buffer());
            self.subgroup_header = Some(SubgroupHeader::from_bytes(&mut b, self.version)?);
            debug!("parsed subgroup header: {:?}", self.subgroup_header);
            self.buf.consume(b.off());
        }

        Ok(())
    }

    /// return `Error::Done` when no header is available right now.
    /// return `Error::Fin` stream has finished.
    fn read_next_object_header(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) -> Result<ObjectHeader> {
        assert_eq!(self.remaining_object_payload, 0);
        let subgroup_header = self.subgroup_header.as_ref().unwrap();

        let object_header = loop {
            let mut b = Octets::with_slice(self.buf.buffer());
            let oh = match ObjectHeader::from_bytes(&mut b, self.version, subgroup_header) {
                Ok(v) => v,
                Err(quiche_moq_wire::Error::Octets(octets::BufferTooShortError)) => {
                    match self.buf.fill(|b| {
                        wt.recv_stream(self.stream_id.into(), self.session_id.into(), h3, quic, b)
                    }) {
                        Ok(_) => {}
                        Err(quiche_webtransport::Error::Done) => return Err(Error::Done),
                        Err(quiche_webtransport::Error::Fin) => return Err(Error::Fin),
                        Err(e) => unimplemented!("{:?}", e),
                    };
                    continue;
                }
                Err(e) => unimplemented!("{:?}", e),
            };
            self.buf.consume(b.off());
            break oh;
        };

        debug!("parsed object header: {:?}", object_header);
        self.remaining_object_payload = object_header.payload_len();
        Ok(object_header)
    }

    pub fn read_obj_hdr(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) -> Result<ObjectHeader> {
        let object_header = self.read_next_object_header(quic, h3, wt)?;
        Ok(object_header)
    }

    /// 0 if the next object header can be read.
    pub fn remaining_object_payload(&self) -> usize {
        self.remaining_object_payload
    }

    pub fn subgroup_header(&self) -> Option<&SubgroupHeader> {
        self.subgroup_header.as_ref()
    }

    /// return Error::Done when no data is available at the moment
    pub fn read_obj_pld(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
        buf: &mut [u8],
    ) -> Result<usize> {
        assert_ne!(self.remaining_object_payload, 0);
        assert_ne!(buf.len(), 0);
        let len = min(buf.len(), self.remaining_object_payload);
        let n = match self.buf.chain_read2(
            |b| wt.recv_stream(self.stream_id.into(), self.session_id.into(), h3, quic, b),
            &mut buf[..len],
        ) {
            Ok(v) => v,
            Err(quiche_webtransport::Error::Done) => return Err(Error::Done),
            Err(e) => unimplemented!("{:?}", e)
        };
        self.remaining_object_payload -= n;
        Ok(n)
    }
}

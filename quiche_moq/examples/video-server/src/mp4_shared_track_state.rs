use log::trace;
use mio::unix::pipe::Receiver;
use mp4::{BoxHeader, HEADER_SIZE};
use short_buf::ShortBuf;
use std::io;
use std::io::{Cursor, Read};

#[derive(Clone, Debug)]
pub(crate) enum State {
    Ftype,
    Moov,
    Box {
        header: Option<BoxHeader>,
        index: usize,
    },
}

pub(crate) struct Mp4SharedTrackState {
    pub state: State,
    pub ftype_box_buf: ShortBuf<100>,
    pub moov_box_buf: ShortBuf<1000>,
    pub box_buf: ShortBuf<100_000>,
}

impl Mp4SharedTrackState {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ftype,
            ftype_box_buf: ShortBuf::new(),
            moov_box_buf: ShortBuf::new(),
            box_buf: ShortBuf::new(),
        }
    }

    pub fn read_next(&mut self, reader: &mut Receiver) -> io::Result<()> {
        loop {
            match &mut self.state {
                State::Ftype => {
                    let buf = &mut self.ftype_box_buf;
                    buf.fill_until(|b| reader.read(b), HEADER_SIZE as usize)?;
                    let header = mp4::BoxHeader::read(&mut Cursor::new(buf.buffer())).unwrap();
                    trace!("header: {:?}", header);
                    assert!(header.size > HEADER_SIZE);
                    assert_eq!(header.name, mp4::BoxType::FtypBox);
                    buf.fill_until(|b| reader.read(b), header.size as usize)?;
                    self.state = State::Moov;
                    continue;
                }
                State::Moov => {
                    let buf = &mut self.moov_box_buf;
                    buf.fill_until(|b| reader.read(b), HEADER_SIZE as usize)?;
                    let header = mp4::BoxHeader::read(&mut Cursor::new(buf.buffer())).unwrap();
                    trace!("header: {:?}", header);
                    assert!(header.size > HEADER_SIZE);
                    assert_eq!(header.name, mp4::BoxType::MoovBox);
                    buf.fill_until(|b| reader.read(b), header.size as usize)?;
                    self.state = State::Box {
                        header: None,
                        index: 0,
                    };
                    continue;
                }
                State::Box { header, index } => {
                    let buf = &mut self.box_buf;
                    let hdr = match header {
                        Some(v) => v,
                        None => {
                            buf.fill_until(|b| reader.read(b), HEADER_SIZE as usize)?;
                            let hdr = mp4::BoxHeader::read(&mut Cursor::new(buf.buffer())).unwrap();
                            trace!("header: {:?}", hdr);
                            assert!(hdr.size > HEADER_SIZE);
                            *header = Some(hdr);
                            header.as_mut().unwrap()
                        }
                    };
                    if buf.len() == hdr.size as usize {
                        // was already read fully, read next
                        trace!(
                            "box finished: index:{} type:{} bytes:{}",
                            index, hdr.name, hdr.size
                        );
                        buf.consume_all();
                        *index += 1;
                        *header = None;
                        continue;
                    }
                    buf.fill_until(|b| reader.read(b), hdr.size as usize)?;
                    return Ok(()); // application can now consume this box
                }
            };
        }
    }

    pub(crate) fn has_ftype(&self) -> bool {
        !matches!(self.state, State::Ftype)
    }

    pub(crate) fn has_moov(&self) -> bool {
        matches!(self.state, State::Box { .. })
    }
}

use crate::mp4_shared_track_state;
use crate::mp4_shared_track_state::Mp4SharedTrackState;
use log::trace;
use quiche_mio_runner::quiche_endpoint::quiche;
use quiche_mio_runner::quiche_endpoint::quiche::h3;
use quiche_moq as moq;
use quiche_moq::wire::TrackAlias;
use quiche_webtransport as wt;

enum State {
    Ftype,
    Moov,
    Box {
        sent_header: bool,
        offset: usize,
        index: usize,
    },
}

pub(crate) struct Mp4TrackState {
    track_alias: TrackAlias,
    state: State,
}

impl Mp4TrackState {
    pub(crate) fn new(track_alias: TrackAlias) -> Self {
        Self {
            track_alias,
            state: State::Ftype,
        }
    }

    pub fn send(
        &mut self,
        gs: &Mp4SharedTrackState,
        moq: &mut moq::MoqTransportSession,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) {
        loop {
            match &mut self.state {
                State::Ftype => {
                    if !gs.has_ftype() {
                        return;
                    }
                    moq.send_obj(gs.ftype_box_buf.buffer(), self.track_alias, wt, h3, quic)
                        .unwrap();
                    self.state = State::Moov;
                }
                State::Moov => {
                    if !gs.has_moov() {
                        return;
                    }
                    moq.send_obj(gs.moov_box_buf.buffer(), self.track_alias, wt, h3, quic)
                        .unwrap();
                    self.state = State::Box {
                        offset: 0,
                        index: 0,
                        sent_header: false,
                    };
                }
                State::Box {
                    offset,
                    index,
                    sent_header,
                } => {
                    let mp4_shared_track_state::State::Box {
                        index: gs_index,
                        header,
                    } = gs.state
                    else {
                        unreachable!()
                    };
                    if *index < gs_index {
                        *index = gs_index;
                        *sent_header = false;
                        *offset = 0;
                        trace!("fast forward box index")
                    }
                    if *index > gs_index {
                        return; // nothing left to send
                    }
                    let Some(header) = header else { return };
                    if !*sent_header {
                        match moq.send_obj_hdr(header.size as usize, self.track_alias, wt, h3, quic)
                        {
                            Ok(_) => {}
                            Err(moq::Error::UnfinishedPayload) => {
                                moq.timeout_stream(self.track_alias, wt, quic);
                                continue;
                            }
                            Err(e) => unimplemented!("{:?}", e),
                        }
                        *sent_header = true;
                    }
                    let n = match moq.send_obj_pld(
                        &gs.box_buf.buffer()[*offset..],
                        self.track_alias,
                        wt,
                        quic,
                    ) {
                        Ok(v) => v,
                        Err(moq::Error::Done) => return,
                        Err(e) => unimplemented!("{:?}", e),
                    };
                    *offset += n;
                    if *offset == header.size as usize {
                        *index += 1;
                        *offset = 0;
                        *sent_header = false;
                    }
                }
            }
        }
    }
}

use quiche_moq::OutgoingObject;
use crate::mp4_shared_track_state;
use crate::mp4_shared_track_state::Mp4SharedTrackState;
use log::trace;
use quiche_moq as moq;
use quiche_moq::wire::{Location, TrackAlias};

/// The whole fMP4 stream (ftype/moov/boxes) is sent as one continuous
/// Group/Subgroup — there's only ever one join point in this simple example.
const GROUP_ID: u64 = 0;
const SUBGROUP_ID: u64 = 0;

enum State {
    /// `sent`: payload bytes sent, `None` until the header is sent
    Ftype { sent: Option<usize> },
    Moov { sent: Option<usize> },
    Box {
        sent_header: bool,
        offset: usize,
        index: usize,
    },
}

/// Sends `buf` as one object, continuing after `sent` payload bytes (`None`:
/// header not sent yet). `true` once complete, `false` to retry later.
fn send_whole_object(
    track_alias: TrackAlias,
    next_object_id: &mut u64,
    sent: &mut Option<usize>,
    buf: &[u8],
    moq: &mut moq::MoqHandle,
) -> bool {
    if sent.is_none() {
        let location = Location { group: GROUP_ID, object: *next_object_id };
        if moq.session.send_obj_hdr(track_alias, OutgoingObject::new(location, SUBGROUP_ID, buf.len()), &mut moq.t).is_err() {
            return false;
        }
        *next_object_id += 1;
        *sent = Some(0);
    }
    let location = Location { group: GROUP_ID, object: *next_object_id - 1 };
    let sent = sent.as_mut().unwrap();
    while *sent < buf.len() {
        match moq.session.send_obj_pld(track_alias, location, &buf[*sent..], &mut moq.t) {
            Ok(n) => *sent += n,
            Err(_) => return false,
        }
    }
    true
}

pub(crate) struct Mp4TrackState {
    track_alias: TrackAlias,
    /// Ends the track when dropped.
    _publication: moq::Publication,
    state: State,
    next_object_id: u64,
}

impl Mp4TrackState {
    pub(crate) fn new(publication: moq::Publication) -> Self {
        Self {
            track_alias: publication.track_alias(),
            _publication: publication,
            state: State::Ftype { sent: None },
            next_object_id: 0,
        }
    }

    pub fn send(
        &mut self,
        gs: &Mp4SharedTrackState,
        moq: &mut moq::MoqHandle,
    ) {
        loop {
            match &mut self.state {
                State::Ftype { sent } => {
                    if !gs.has_ftype()
                        || !send_whole_object(self.track_alias, &mut self.next_object_id, sent, gs.ftype_box_buf.buffer(), moq)
                    {
                        return;
                    }
                    self.state = State::Moov { sent: None };
                }
                State::Moov { sent } => {
                    if !gs.has_moov()
                        || !send_whole_object(self.track_alias, &mut self.next_object_id, sent, gs.moov_box_buf.buffer(), moq)
                    {
                        return;
                    }
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
                    let object_id = self.next_object_id;
                    let location = Location { group: GROUP_ID, object: object_id };
                    if !*sent_header {
                        match moq.session.send_obj_hdr(self.track_alias, OutgoingObject::new(location, SUBGROUP_ID, header.size as usize), &mut moq.t) {
                            Ok(()) => { self.next_object_id += 1; }
                            Err(moq::Error::UnfinishedPayload) => {
                                moq.session.reset_send_subgroup(self.track_alias, GROUP_ID, SUBGROUP_ID, moq::wire::ResetCode::Canceled, &mut moq.t);
                                continue;
                            }
                            Err(e) => unimplemented!("{:?}", e),
                        }
                        *sent_header = true;
                    }
                    let n = match moq.session.send_obj_pld(
                        self.track_alias,
                        location,
                        &gs.box_buf.buffer()[*offset..], &mut moq.t) {
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

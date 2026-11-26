use moq_core::session::{MoqTransportSession, RecvObjHdr};
use moq_core::transport::Transport;
use moq_core::wire::{Location, TrackAlias};
use moq_core::{DropQueue, Result};

/// A lightweight, policy-free handle for reading one track's raw data.
/// Caller-driven: pass in the session and transport (e.g. via
/// `AsyncMoqSessionActor::parts_mut()`) on every call, same as calling
/// `MoqTransportSession::recv_new_obj_hdr`/`recv_obj_pld` directly — this
/// just bundles `track_alias` so callers don't repeat it.
/// No reassembly, no buffering, no Subgroup bookkeeping (the session
/// already hides that); see [`crate::BufferedTrack`] for reassembled,
/// complete objects.
pub struct InTrack {
    track_alias: TrackAlias,
}

impl InTrack {
    pub fn new(track_alias: TrackAlias) -> Self {
        Self { track_alias }
    }

    pub fn new_obj_hdr<Q: DropQueue>(&mut self, session: &mut MoqTransportSession<Q>, t: &mut impl Transport) -> Result<RecvObjHdr> {
        session.recv_new_obj_hdr(self.track_alias, t)
    }

    pub fn obj_pld<Q: DropQueue>(&mut self, location: Location, session: &mut MoqTransportSession<Q>, t: &mut impl Transport, dest: &mut [u8]) -> Result<usize> {
        session.recv_obj_pld(self.track_alias, location, dest, t)
    }

    /// See `MoqTransportSession::cancel_recv_obj`.
    pub fn cancel_obj<Q: DropQueue>(&mut self, location: Location, session: &mut MoqTransportSession<Q>, t: &mut impl Transport) {
        session.cancel_recv_obj(self.track_alias, location, t);
    }
}

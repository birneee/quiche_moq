use moq_core::session::MoqTransportSession;
use moq_core::transport::Transport;
use moq_core::wire::object::ObjectHeader;
use moq_core::wire::TrackAlias;
use moq_core::{Error, Result};

/// One raw read from a track: either a new object's header, or a chunk of
/// the current object's payload. No reassembly, no buffering — just
/// `MoqTransportSession::read_obj_hdr`/`read_obj_pld` through one method
/// that tracks which of the two comes next. See [`crate::BufferedTrack`]
/// for reassembled, complete objects.
pub enum ObjectPart {
    /// A new object's header. `group_id` is `None` if the current subgroup
    /// stream's header isn't available (e.g. the stream just changed) —
    /// deciding what to do about that (fall back to the previous group?
    /// drop the object?) is a buffering policy, left to the caller.
    Header { header: ObjectHeader, group_id: Option<u64> },
    /// A chunk of the payload belonging to the most recently returned
    /// `Header`, written into the `dest` slice passed to `read_part` — the
    /// value is how many bytes of `dest` were filled. Reading continues to
    /// return `Payload` until the header's `payload_len()` bytes have all
    /// been delivered, then switches back to `Header` for the next object.
    Payload(usize),
}

/// A lightweight, policy-free handle for reading one track's raw data.
/// Caller-driven: pass in the session and transport (e.g. via
/// `AsyncMoqSessionActor::parts_mut()`) on every call, same as calling
/// `read_obj_hdr`/`read_obj_pld` directly — this only tracks the
/// header/payload state machine so the caller doesn't have to. Payload
/// bytes are written directly into a caller-supplied `dest` rather than a
/// freshly allocated buffer, so a caller with somewhere to put them (e.g.
/// `BufferedTrack` writing straight into its object cache) doesn't pay for
/// an extra allocation and copy per chunk.
pub struct Track {
    track_alias: TrackAlias,
    /// Bytes still owed for the object whose header was last returned; 0
    /// means the next `read_part()` call reads a new header.
    remaining_payload: usize,
}

impl Track {
    pub fn new(track_alias: TrackAlias) -> Self {
        Self { track_alias, remaining_payload: 0 }
    }

    /// Reads one header or one chunk of payload (up to `dest.len()` bytes),
    /// whichever is next. `dest` is only touched when returning `Payload`;
    /// pass an empty slice if the caller has no destination ready and
    /// expects a `Header` (e.g. right after delivering a complete object).
    /// `Err(Error::Done)` means nothing is available right now — try again
    /// after the next wake; `Err(Error::Fin)` means the track's stream
    /// ended.
    pub fn read_part(&mut self, session: &mut MoqTransportSession, t: &mut impl Transport, dest: &mut [u8]) -> Result<ObjectPart> {
        if self.remaining_payload == 0 {
            let header = session.read_obj_hdr(self.track_alias, t)?;
            self.remaining_payload = header.payload_len();
            let group_id = session.subgroup_header(self.track_alias).map(|sg| sg.group_id());
            Ok(ObjectPart::Header { header, group_id })
        } else {
            let want = self.remaining_payload.min(dest.len());
            let n = session.read_obj_pld(&mut dest[..want], self.track_alias, t)?;
            self.remaining_payload -= n;
            if n == 0 {
                return Err(Error::Done);
            }
            Ok(ObjectPart::Payload(n))
        }
    }
}

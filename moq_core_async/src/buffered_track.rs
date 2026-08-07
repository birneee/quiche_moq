use moq_core::session::MoqTransportSession;
use moq_core::transport::Transport;
use moq_core::wire::{KeyValuePairs, Location, TrackAlias};
use moq_core::Error;
use moq_object_cache::{ObjectCache, PackedObjectCache};

use crate::track::{ObjectPart, Track};

/// A complete, reassembled object — the payload may have arrived across
/// several network reads; `BufferedTrack` only hands this back once
/// `payload.len()` matches what the header declared.
pub struct CompleteObject {
    pub location: Location,
    pub ext_hdrs: KeyValuePairs,
    pub payload: Vec<u8>,
}

/// Reassembles objects off a [`Track`] using `moq_object_cache`, same
/// pattern `moq_web`'s decode loop and `moq_cli`'s relay both hand-roll —
/// centralized here so callers that just want "give me complete objects"
/// don't reimplement it (and don't reproduce the truncation bug an earlier,
/// cache-less version of `moq_tokio`'s loop had: treating `Error::Done` —
/// "no more bytes *right now*" — as "object finished").
///
/// Not the only valid way to read a track: use [`Track`] directly for
/// partial/streaming access without this buffering policy.
pub struct BufferedTrack {
    track: Track,
    cache: PackedObjectCache,
    /// Falls back to this when a subgroup stream's header isn't available
    /// for the current object (e.g. between streams) — same fallback
    /// `moq_web`/`moq_cli` use.
    last_group: u64,
    /// Payload bytes still owed by a header whose location `cache.begin`
    /// rejected (out-of-order — possible when a track's objects arrive
    /// across multiple interleaved streams/subgroups). Must still be read
    /// off the wire even though there's nowhere to put it.
    discarding: Option<usize>,
    last_delivered: Option<Location>,
    /// Read target for payload chunks that must be discarded (no cache slot
    /// to write into) — `Track::read_part` still needs a real, non-empty
    /// `dest` to know how many bytes it may read. The common case (a cache
    /// slot exists) writes straight into `obj.unfilled()` instead, so this
    /// is only ever touched on the out-of-order/discard path.
    discard_scratch: Vec<u8>,
}

impl BufferedTrack {
    /// `max_objects`/`max_object_size`/`average_object_size` configure the
    /// underlying `PackedObjectCache` — see its own docs for what these
    /// trade off (this is exactly the buffering *policy* `moq_core` itself
    /// deliberately doesn't impose; callers who want a different one should
    /// use `Track` directly instead of this wrapper).
    pub fn new(track_alias: TrackAlias, max_objects: usize, max_object_size: usize, average_object_size: usize) -> Self {
        Self {
            track: Track::new(track_alias),
            cache: PackedObjectCache::new(max_objects, max_object_size, average_object_size),
            last_group: 0,
            discarding: None,
            last_delivered: None,
            discard_scratch: vec![0u8; 64 << 10],
        }
    }

    /// `max_objects: 8` (the cache's object capacity — how many in-flight
    /// objects it can hold at once, not a byte size), `max_object_size:
    /// 4 MiB`, `average_object_size: 256 KiB` (used to size the backing
    /// buffer up front: `max_objects * average_object_size`). Both
    /// `moq_web` and `moq_tokio` used these exact numbers independently
    /// before this was centralized — start here, and call `new()` directly
    /// only if a track actually needs different sizing.
    pub fn with_defaults(track_alias: TrackAlias) -> Self {
        Self::new(track_alias, 8, 4 << 20, 256 << 10)
    }

    /// Reads as much as is currently available, returning the next newly
    /// completed object if reassembly finished one — `None` if nothing is
    /// ready yet (call again after the next wake) or if the track's stream
    /// ended.
    pub fn poll_object(&mut self, session: &mut MoqTransportSession, t: &mut impl Transport) -> Option<CompleteObject> {
        loop {
            if let Some(remaining) = self.discarding {
                if remaining == 0 {
                    self.discarding = None;
                    continue;
                }
            }

            // Payload chunks are written straight into the cache slot's own
            // buffer when one exists — only the discard path needs the
            // scratch fallback. `read_part` ignores `dest` entirely when
            // it's about to return a `Header` instead.
            let part = if self.discarding.is_none() && self.cache.latest_mut().is_some() {
                let mut obj = self.cache.latest_mut().unwrap();
                self.track.read_part(session, t, obj.unfilled())
            } else {
                self.track.read_part(session, t, &mut self.discard_scratch)
            };

            match part {
                Ok(ObjectPart::Header { header, group_id }) => {
                    if header.status().is_some() {
                        continue;
                    }
                    let group = group_id.unwrap_or(self.last_group);
                    self.last_group = group;
                    let loc = Location { group, object: header.id() };
                    if self
                        .cache
                        .begin(loc, header.extension_headers().clone(), header.payload_len())
                        .is_err()
                    {
                        log::warn!("dropping out-of-order object at {loc:?}");
                        self.discarding = (header.payload_len() > 0).then_some(header.payload_len());
                    } else {
                        self.discarding = None;
                    }
                }
                Ok(ObjectPart::Payload(n)) => {
                    if let Some(remaining) = &mut self.discarding {
                        *remaining = remaining.saturating_sub(n);
                    } else if let Some(mut obj) = self.cache.latest_mut() {
                        obj.advance_write_pos(n);
                    }
                }
                Err(Error::Fin) => {
                    // The stream ended mid-object — no more bytes are ever
                    // coming for it, so mark it complete-as-is rather than
                    // leaving it stuck incomplete forever.
                    if let Some(mut obj) = self.cache.latest_mut() {
                        obj.cancel();
                    }
                    return None;
                }
                Err(_) => return None,
            }

            if let Some(obj) = self.cache.latest() {
                if obj.is_complete() && self.last_delivered != Some(obj.location()) {
                    self.last_delivered = Some(obj.location());
                    return Some(CompleteObject {
                        location: obj.location(),
                        ext_hdrs: obj.ext_hdrs().clone(),
                        payload: obj.readable_from(0).to_vec(),
                    });
                }
            }
        }
    }
}

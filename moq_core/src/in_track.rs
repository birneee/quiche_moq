use log::debug;
use smallvec::SmallVec;
use moq_wire::TrackAlias;

pub(crate) struct InTrack {
    track_alias: TrackAlias,
    /// Readable stream ids, sorted ascending.
    readable_streams: SmallVec<u64, 10>,
    /// Number of streams fin'd so far.
    finned_streams: u64,
    /// Total streams expected, set when PUBLISH_DONE is received.
    expected_streams: Option<u64>,
}

impl InTrack {
    pub fn new(track_alias: TrackAlias) -> Self {
        Self {
            track_alias,
            readable_streams: SmallVec::new(),
            finned_streams: 0,
            expected_streams: None,
        }
    }

    pub(crate) fn mark_done(&mut self, stream_count: u64) {
        self.expected_streams = Some(stream_count);
    }

    pub(crate) fn is_fully_done(&self) -> bool {
        self.expected_streams.is_some_and(|ec| self.finned_streams >= ec)
    }

    pub fn readable(&self) -> bool {
        !self.readable_streams.is_empty()
    }

    pub fn readable_streams(&self) -> &[u64] {
        &self.readable_streams
    }

    pub(crate) fn fin_stream(&mut self, stream_id: u64) {
        if let Some(pos) = self.readable_streams.iter().position(|&i| i == stream_id) {
            self.readable_streams.remove(pos);
            self.finned_streams += 1;
        }
    }

    pub fn mark_stream_readable(&mut self, stream_id: u64) {
        debug!("track {} readable stream {}", self.track_alias, stream_id);
        match self.readable_streams.binary_search(&stream_id) {
            Ok(_) => {}
            Err(pos) => self.readable_streams.insert(pos, stream_id),
        }
    }

    pub(crate) fn current_stream(&self) -> Option<u64> {
        self.readable_streams.first().copied()
    }
}

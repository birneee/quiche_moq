use log::debug;
use smallvec::SmallVec;
use quiche_moq_wire::TrackAlias;
use quiche_utils::stream_id::StreamID;

pub(crate) struct InTrack {
    track_alias: TrackAlias,
    // this is sorted ascending
    readable_streams: SmallVec<StreamID, 10>,
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

    /// Called when PUBLISH_DONE is received; `stream_count` is the total streams the peer sent.
    pub(crate) fn mark_done(&mut self, stream_count: u64) {
        self.expected_streams = Some(stream_count);
    }

    /// True when all expected streams have been fully consumed.
    pub(crate) fn is_fully_done(&self) -> bool {
        self.expected_streams.is_some_and(|ec| self.finned_streams >= ec)
    }

    pub fn readable(&self) -> bool {
        !self.readable_streams.is_empty()
    }

    pub fn readable_streams(&self) -> &[StreamID] {
        &self.readable_streams
    }

    /// mark stream reading as finished, no more objects will be read from this stream
    pub(crate) fn fin_stream(&mut self, stream_id: StreamID) {
        if let Some(pos) = self.readable_streams.iter().position(|i| *i == stream_id) {
            self.readable_streams.remove(pos);
            self.finned_streams += 1;
        }
    }

    pub fn mark_stream_readable(&mut self, stream_id: StreamID) {
        debug!("track {} readable stream {}", self.track_alias, stream_id);
        match self.readable_streams.binary_search(&stream_id) {
            Ok(_) => {} // already contained
            Err(pos) => self.readable_streams.insert(pos, stream_id),
        }
    }

    pub(crate) fn current_stream(&self) -> Option<StreamID> {
        self.readable_streams.first().copied()
    }
}

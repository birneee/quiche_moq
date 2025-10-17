use log::debug;
use smallvec::SmallVec;
use quiche_moq_wire::TrackAlias;
use quiche_utils::stream_id::StreamID;

pub(crate) struct InTrack {
    track_alias: TrackAlias,
    // this is sorted ascending
    readable_streams: SmallVec<StreamID, 10>,
}

impl InTrack {
    pub fn new(track_alias: TrackAlias) -> Self {
        Self {
            track_alias,
            readable_streams: SmallVec::new(),
        }
    }

    pub fn readable(&self) -> bool {
        !self.readable_streams.is_empty()
    }

    pub fn readable_streams(&self) -> &[StreamID] {
        &self.readable_streams
    }

    /// mark stream reading as finished, no more objects will be read from this stream
    pub fn fin_stream(&mut self, stream_id: StreamID) {
        self.readable_streams.retain(|i| *i != stream_id);
    }

    pub fn mark_stream_readable(&mut self, stream_id: StreamID) {
        debug!("track {} readable stream {}", self.track_alias, stream_id);
        match self.readable_streams.binary_search(&stream_id) {
            Ok(_) => {} // already contained
            Err(pos) => self.readable_streams.insert(pos, stream_id),
        }
    }

    pub(crate) fn current_stream(&self) -> Option<StreamID> {
        self.readable_streams.first().map(|s| *s)
    }
}

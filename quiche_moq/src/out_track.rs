use quiche_utils::stream_id::StreamID;

/// Track state of an egress subscription
pub struct OutTrack {
    pub(crate) current_stream_id: Option<StreamID>,
}

impl OutTrack {
    pub fn new() -> Self {
        Self {
            current_stream_id: None,
        }
    }

    pub fn writable(&self) -> bool {
        true
    }
}

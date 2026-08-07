use moq_wire::TrackAlias;

/// Represents a pending subscribe that the peer has not yet answered.
pub(crate) struct PendingSubscribe {
    /// `Some` for draft ≤ 11 (the subscriber suggests the alias); `None` for draft ≥ 12.
    track_alias: Option<TrackAlias>,
}

impl PendingSubscribe {
    pub fn new(track_alias: Option<TrackAlias>) -> Self {
        Self { track_alias }
    }

    pub fn track_alias(&self) -> Option<TrackAlias> {
        self.track_alias
    }
}

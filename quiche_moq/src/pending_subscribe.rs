use quiche_moq_wire::TrackAlias;

/// Represents a pending subscribe that is not answered by the peer yet.
pub(crate) struct PendingSubscribe {
    /// Is `Some` for draft 11.
    /// Is `None` for draft 13.
    track_alias: Option<TrackAlias>,
}

impl PendingSubscribe {
    pub fn new(track_alias: Option<TrackAlias>) -> Self {
        Self {
            track_alias,
        }
    }

    /// Is `Some` for draft 11.
    /// Is `None` for draft 13.
    pub fn track_alias(&self) -> Option<TrackAlias> {
        self.track_alias
    }
}

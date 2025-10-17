use quiche_moq_wire::{Version, MOQ_VERSION_DRAFT_13};

#[derive(Copy, Clone)]
pub struct Config {
    /// The version to use to send the client setup message
    pub setup_version: Version,
    pub ignore_max_request_quota: bool
}

impl Default for Config {
    fn default() -> Self {
        Self {
            setup_version: MOQ_VERSION_DRAFT_13,
            ignore_max_request_quota: false,
        }
    }
}

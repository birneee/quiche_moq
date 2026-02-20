use quiche_moq_wire::{MOQ_VERSION_DRAFT_16, SUPPORTED_MOQ_VERSIONS, Version};

#[derive(Clone)]
pub struct Config {
    /// The version to use to send the client setup message
    pub setup_version: Version,
    pub supported_versions: Vec<Version>,
    pub ignore_max_request_quota: bool
}

impl Default for Config {
    fn default() -> Self {
        Self {
            setup_version: MOQ_VERSION_DRAFT_16,
            supported_versions: SUPPORTED_MOQ_VERSIONS.to_vec(),
            ignore_max_request_quota: false,
        }
    }
}

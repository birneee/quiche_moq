use quiche_moq_wire::{Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_LITE_01_BY_KIXELATED};

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
            setup_version: MOQ_VERSION_DRAFT_13,
            supported_versions: vec![
                MOQ_VERSION_DRAFT_07,
                MOQ_VERSION_DRAFT_08,
                MOQ_VERSION_DRAFT_09,
                MOQ_VERSION_DRAFT_10,
                MOQ_VERSION_DRAFT_11,
                MOQ_VERSION_DRAFT_12,
                MOQ_VERSION_DRAFT_13,
                MOQ_VERSION_LITE_01_BY_KIXELATED
            ],
            ignore_max_request_quota: false,
        }
    }
}

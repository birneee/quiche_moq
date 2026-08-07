pub(crate) struct PendingRequest {
    session_id: u64,
    /// Protocols this request offered via `WT-Available-Protocols`, most
    /// preferred first. Empty if the request didn't include the header.
    available_protocols: Vec<String>,
}

impl PendingRequest {
    pub(crate) fn new(available_protocols: Vec<String>) -> Self {
        Self { session_id: 0, available_protocols }
    }

    pub(crate) fn session_id(&self) -> u64 {
        self.session_id
    }

    pub(crate) fn available_protocols(&self) -> &[String] {
        &self.available_protocols
    }
}

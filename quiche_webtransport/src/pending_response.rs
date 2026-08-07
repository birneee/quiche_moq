pub(crate) struct PendingResponse {
    session_id: u64,
    /// Protocols the client offered via `WT-Available-Protocols` on its
    /// CONNECT request, most preferred first. Empty if it didn't send one.
    available_protocols: Vec<String>,
    /// Set by the application (via `Connection::set_negotiated_protocol`)
    /// to echo back as `WT-Protocol` in the 2xx response, once it's picked
    /// one of `available_protocols`.
    response_protocol: Option<String>,
}

impl PendingResponse {
    pub(crate) fn new(session_id: u64, available_protocols: Vec<String>) -> Self {
        Self { session_id, available_protocols, response_protocol: None }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub(crate) fn available_protocols(&self) -> &[String] {
        &self.available_protocols
    }

    pub(crate) fn response_protocol(&self) -> Option<&str> {
        self.response_protocol.as_deref()
    }

    pub(crate) fn set_response_protocol(&mut self, protocol: String) {
        self.response_protocol = Some(protocol);
    }
}

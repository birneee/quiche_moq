pub(crate) struct PendingRequest {
    session_id: u64,
}

impl PendingRequest {
    pub(crate) fn new() -> Self {
        Self { session_id: 0 }
    }
    
    pub(crate) fn session_id(&self) -> u64 {
        self.session_id
    }
}

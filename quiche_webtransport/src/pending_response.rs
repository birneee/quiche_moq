pub(crate) struct PendingResponse {
    session_id: u64,
}

impl PendingResponse {
    pub(crate) fn new(session_id: u64) -> Self {
        Self {
            session_id,
        }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }
}
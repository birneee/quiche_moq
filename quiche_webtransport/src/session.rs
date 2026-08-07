#[allow(unused)]
pub struct Session {
    state: State,
    /// The protocol negotiated via `WT-Available-Protocols`/`WT-Protocol`
    /// (draft-ietf-webtrans-http3 §3.3), if any — `None` if either side
    /// didn't participate in that negotiation.
    negotiated_protocol: Option<String>,
}

#[allow(unused)]
enum State {
    Pending,
    HttpError,
    Established,
}

impl Session {
    #[allow(unused)]
    pub fn connect(_session_id: u64, negotiated_protocol: Option<String>) -> Self {
        Self {
            state: State::Pending,
            negotiated_protocol,
        }
    }

    pub fn accept(_session_id: u64, negotiated_protocol: Option<String>) -> Self {
        Self {
            state: State::Established,
            negotiated_protocol,
        }
    }

    pub fn negotiated_protocol(&self) -> Option<&str> {
        self.negotiated_protocol.as_deref()
    }

    pub(crate) fn set_negotiated_protocol(&mut self, protocol: String) {
        self.negotiated_protocol = Some(protocol);
    }
}

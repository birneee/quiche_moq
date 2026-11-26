#[allow(unused)]
pub struct Session {
    state: State,
    /// The protocol negotiated via `WT-Available-Protocols`/`WT-Protocol`
    /// (draft-ietf-webtrans-http3 §3.3), if any — `None` if either side
    /// didn't participate in that negotiation.
    negotiated_protocol: Option<String>,
    /// capsules arriving on the CONNECT stream
    pub(crate) capsules: http_capsule::Decoder,
    /// error code and message of the peer's WT_CLOSE_SESSION
    pub(crate) peer_close: Option<(u32, String)>,
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
            capsules: http_capsule::Decoder::new(4 + crate::connection::MAX_CLOSE_MESSAGE_LEN),
            peer_close: None,
        }
    }

    pub fn accept(_session_id: u64, negotiated_protocol: Option<String>) -> Self {
        Self {
            state: State::Established,
            negotiated_protocol,
            capsules: http_capsule::Decoder::new(4 + crate::connection::MAX_CLOSE_MESSAGE_LEN),
            peer_close: None,
        }
    }

    pub fn negotiated_protocol(&self) -> Option<&str> {
        self.negotiated_protocol.as_deref()
    }

    pub(crate) fn set_negotiated_protocol(&mut self, protocol: String) {
        self.negotiated_protocol = Some(protocol);
    }
}

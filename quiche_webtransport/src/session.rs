#[allow(unused)]
pub struct Session {
    state: State
}

#[allow(unused)]
enum State {
    Pending,
    HttpError,
    Established,
}

impl Session {
    #[allow(unused)]
    pub fn connect(_session_id: u64) -> Self {
        Self {
            state: State::Pending,
        }
    }

    pub fn accept(_session_id: u64) -> Self {
        Self {
            state: State::Established,
        }
    }
}

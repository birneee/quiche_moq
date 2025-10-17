use std::fmt::Display;

#[derive(Hash, Eq, PartialEq, Debug, Clone, Copy, Ord, PartialOrd)]
pub struct StreamID(u64);

impl StreamID {
    pub fn is_bidi(&self) -> bool {
        (self.0 & 0x2) == 0
    }

    pub fn is_local(&self, is_server: bool) -> bool {
        (self.0 & 0x1) == (is_server as u64)
    }

    pub fn into_u64(self) -> u64 {
        self.0
    }
}

impl Into<u64> for StreamID {
    fn into(self) -> u64 {
        self.0
    }
}

impl From<u64> for StreamID {
    fn from(id: u64) -> Self {
        StreamID(id)
    }
}

impl Display for StreamID {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(f, "{}", self.0)
    }
}

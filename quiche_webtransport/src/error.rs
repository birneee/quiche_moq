pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Done,
    MissingSessionID,
    /// Stream state does not yet exist or is already cleaned up.
    NoStreamState,
    InsufficientCapacity,
    Fin,
}

use crate::prelude::*;
use crate::transport::StreamError;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Unimplemented,

    Done,
    Fin,
    InsufficientCapacity,
    UnfinishedPayload,
    ExceededPayload,
    /// Insufficient MAX_REQUEST_ID quota from peer, or MAX_REQUEST_UPDATES on the request's stream.
    RequestBlocked,
    /// Object is older than the subscriber's delivery_timeout or max_cache_duration; not sent.
    ObjectExpired,
    /// Object doesn't pass the subscriber's Range Filters (draft-ietf-moq-transport-19 §5.1.3); not sent.
    ObjectFiltered,
    /// `send_obj_pld` targeted an object with
    /// no open stream — never opened, or already finished/reset/expired.
    /// Distinct from `Done` (no capacity *right now*, try again): this is
    /// permanent for this subgroup, and callers with their own per-subgroup
    /// state (e.g. an in-flight-send table) should drop it rather than retry.
    SubgroupNotOpen,
    /// `send_obj_hdr` for a Subgroup whose stream was already FIN'd: a FIN
    /// declares it complete, only a reset lets it continue on a new stream
    /// (draft-16 §2.2). Permanent, drop the object.
    SubgroupFinished,
    /// `send_obj_hdr`'s `prior_object_id_gap` was rejected: larger than
    /// `object_id` itself, retroactively covering an object_id already sent on
    /// this track, or `properties` already carrying one — see
    /// draft-19 §12.9's malformed-track rules.
    InvalidGap,
    /// The egress track no longer exists or was ended: UNSUBSCRIBE received,
    /// PUBLISH_DONE sent or the PUBLISH rejected. Also a subscription whose
    /// request stream ended, there is nothing to update. Permanent, stop sending.
    TrackEnded,
    /// The peer withdrew the request before it was answered. Permanent,
    /// forget the request.
    RequestCancelled,
    /// The session is closed, nothing reaches the peer anymore. Permanent.
    SessionClosed,
    /// The same Object (Group ID + Object ID) was received more than once
    /// with a different payload length — draft-19 §2.4.2 malformed-track
    /// condition 6. The caller must cancel the subscription/fetch for this
    /// track from this publisher and, if relaying, terminate downstream
    /// subscriptions with PUBLISH_DONE/MALFORMED_TRACK.
    MalformedTrack,
    Wire(moq_wire::Error),
    Transport(StreamError),
    InvalidNamespaceTrackname(String),
}

impl From<moq_wire::Error> for Error {
    fn from(err: moq_wire::Error) -> Self {
        Error::Wire(err)
    }
}

impl From<StreamError> for Error {
    fn from(err: StreamError) -> Self {
        match err {
            StreamError::Done => Error::Done,
            StreamError::Fin => Error::Fin,
            StreamError::InsufficientCapacity => Error::InsufficientCapacity,
            other => Error::Transport(other),
        }
    }
}

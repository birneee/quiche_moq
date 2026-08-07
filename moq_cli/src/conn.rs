use std::collections::HashMap;

use bytes::BytesMut;
use quiche_moq::wire::control_message::subscribe::FilterType;
use quiche_moq::wire::{Namespace, NamespaceTrackname, RequestId, TrackAlias};
use quiche_moq::wire::object::ObjectHeader;
use quiche_moq_webtransport_helper::MoqWebTransportHelper;
use quiche_utils::keepalive_handler::KeepaliveHandler;

/// Per-connection state for an actively-PUBLISHed track (--pub ns--track).
pub(crate) struct ClientActivePubTrack {
    pub(crate) track: NamespaceTrackname,
    pub(crate) request_id: Option<RequestId>,
}

pub(crate) struct ClientSubTrack {
    pub(crate) track: NamespaceTrackname,
    pub(crate) request_id: Option<RequestId>,
    pub(crate) track_alias: Option<TrackAlias>,
    pub(crate) obj_hdr: Option<ObjectHeader>,
    pub(crate) obj_buf: BytesMut,
    /// True for --sub-passive tracks: don't send SUBSCRIBE, wait for peer's PUBLISH.
    pub(crate) passive: bool,
    /// From the `--sub`/`--sub-passive` arg's optional `?priority=N` option.
    pub(crate) priority: Option<u8>,
    /// From the `--sub`/`--sub-passive` arg's optional `?delivery_timeout=N` option.
    pub(crate) delivery_timeout: Option<u64>,
    /// From the `--sub`/`--sub-passive` arg's optional `?filter=F` option
    /// (None defaults to NextGroupStart at SUBSCRIBE time).
    pub(crate) filter: Option<FilterType>,
}

impl ClientSubTrack {
    /// Creates a sub-track entry for an already-accepted incoming PUBLISH,
    /// where the peer chose the alias and we just need to start reading objects.
    pub(crate) fn from_accepted_publish(track: NamespaceTrackname, alias: TrackAlias) -> Self {
        Self {
            track,
            request_id: Some(0),
            track_alias: Some(alias),
            obj_hdr: None,
            obj_buf: BytesMut::new(),
            passive: true,
            priority: None,
            delivery_timeout: None,
            filter: None,
        }
    }

    pub(crate) fn from_template(t: &SubTrackTemplate) -> Self {
        Self {
            track: t.track.clone(),
            request_id: None,
            track_alias: None,
            obj_hdr: None,
            obj_buf: BytesMut::new(),
            passive: t.passive,
            priority: t.priority,
            delivery_timeout: t.delivery_timeout,
            filter: t.filter,
        }
    }
}

/// Per-connection template for a `--sub`/`--sub-passive` track, used to
/// (re-)populate `client_sub_tracks` for each new connection.
#[derive(Clone)]
pub(crate) struct SubTrackTemplate {
    pub(crate) track: NamespaceTrackname,
    pub(crate) passive: bool,
    pub(crate) priority: Option<u8>,
    pub(crate) delivery_timeout: Option<u64>,
    pub(crate) filter: Option<FilterType>,
}

pub(crate) struct ConnAppData {
    pub(crate) moq_helper: MoqWebTransportHelper,
    pub(crate) announced_namespaces: HashMap<Namespace, RequestId>,
    pub(crate) logged_connect: bool,
    /// Only set for outbound connections (--connect / --upstream); None for accepted connections.
    pub(crate) keepalive: Option<KeepaliveHandler>,
}

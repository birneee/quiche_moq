use quiche::h3;
use quiche_moq as moq;
use quiche_moq::MoqTransportSession;
use quiche_moq::wire::{NamespaceTrackname, Namespace, RequestId, TrackAlias};
use quiche_moq::wire::control_message::{PublishNamespaceMessage, RequestErrorMessage, SubscribeMessage, SubscribeOkMessage};
use quiche_moq::wire::object::ObjectHeader;
use quiche_webtransport as wt;

/// Temporary handle that bundles all connection references for ergonomic API calls.
/// Created by calling `MoqWebTransportHelper::moq_handle()`.
pub struct MoqHandle<'a> {
    pub session: &'a mut MoqTransportSession,
    pub(crate) quic: &'a mut quiche::Connection,
    pub(crate) h3: &'a mut h3::Connection,
    pub(crate) wt: &'a mut wt::Connection,
}

impl<'a> MoqHandle<'a> {
    /// Subscribe to a track
    pub fn subscribe(&mut self, namespace_trackname: &NamespaceTrackname) -> moq::Result<RequestId> {
        self.session.subscribe(self.quic, self.wt, namespace_trackname)
    }

    /// Poll for a subscribe response
    pub fn poll_subscribe_response(&mut self, request_id: RequestId) -> Option<core::result::Result<(TrackAlias, SubscribeOkMessage), RequestErrorMessage>> {
        self.session.poll_subscribe_response(request_id)
    }

    /// Send a complete MoQ object on a track
    pub fn send_obj(&mut self, buf: &[u8], track_alias: TrackAlias) -> moq::Result<()> {
        self.session.send_obj(buf, track_alias, self.wt, self.h3, self.quic)
    }

    /// Send just the object header (for streaming large objects)
    pub fn send_obj_hdr(&mut self, size: usize, track_alias: TrackAlias) -> moq::Result<()> {
        self.session.send_obj_hdr(size, track_alias, self.wt, self.h3, self.quic)
    }

    /// Send object payload (after sending header)
    pub fn send_obj_pld(&mut self, buf: &[u8], track_alias: TrackAlias) -> moq::Result<usize> {
        self.session.send_obj_pld(buf, track_alias, self.wt, self.quic)
    }

    /// Read an object header from a track
    pub fn read_obj_hdr(&mut self, track_alias: TrackAlias) -> moq::Result<ObjectHeader> {
        self.session.read_obj_hdr(track_alias, self.wt, self.h3, self.quic)
    }

    /// Read object payload (after reading header)
    pub fn read_obj_pld(&mut self, buf: &mut [u8], track_alias: TrackAlias) -> moq::Result<usize> {
        self.session.read_obj_pld(buf, track_alias, self.wt, self.h3, self.quic)
    }

    /// Get a pending subscription request from the peer if available.
    pub fn subscription_inbox_next(&self) -> Option<(&RequestId, &SubscribeMessage)> {
        self.session.subscription_inbox_next()
    }

    /// Accept a subscription and create an outgoing track
    pub fn accept_subscription(&mut self, request_id: RequestId) -> TrackAlias {
        self.session.accept_subscription(self.quic, self.wt, request_id)
    }

    /// Reject a subscription from the peer
    pub fn reject_subscription(&mut self, request_id: RequestId, error_code: u64) {
        self.session.reject_subscription(self.quic, self.wt, request_id, error_code)
    }

    /// Get the next pending namespace publish request
    pub fn next_pending_namespace_publish(&mut self) -> Option<(&RequestId, &PublishNamespaceMessage)> {
        self.session.next_pending_namespace_publish()
    }

    /// Accept a namespace publish request
    pub fn accept_namespace_publish(&mut self, request_id: RequestId) {
        self.session.accept_namespace_publish(request_id, self.quic, self.wt)
    }

    /// Publish a namespace
    pub fn publish_namespace(&mut self, namespace: Vec<Vec<u8>>) -> moq::Result<()> {
        self.session.publish_namespace(self.quic, self.wt, namespace)
    }

    /// Check publish status of a namespace
    pub fn publish_namespace_status(&self, namespace: &Namespace) -> moq::PublishStatus {
        self.session.publish_namespace_status(namespace)
    }

    /// Cancel sending on stream with Delivery Timeout
    pub fn timeout_stream(&mut self, track_alias: TrackAlias) {
        self.session.timeout_stream(track_alias, self.wt, self.quic)
    }

    /// Get remaining payload bytes for current object
    pub fn remaining_object_payload(&self, track_alias: TrackAlias) -> moq::Result<usize> {
        self.session.remaining_object_payload(track_alias)
    }

    /// Get readable track aliases
    pub fn readable(&self) -> smallvec::SmallVec<TrackAlias, 8> {
        self.session.readable()
    }

    /// Get writable track aliases
    pub fn writable(&self) -> smallvec::SmallVec<TrackAlias, 8> {
        self.session.writable()
    }
}

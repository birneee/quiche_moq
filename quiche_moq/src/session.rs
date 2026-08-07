use crate::config::Config;
use crate::error::Result;
use crate::transport::QuicheTransport;
use quiche::h3;
use moq_wire::control_message::subscribe::{FilterType, SubscribeMessage};
use moq_wire::control_message::{
    PublishMessage, PublishNamespaceMessage, PublishOkMessage, RequestErrorMessage,
    SubscribeOkMessage,
};
use moq_wire::object::ObjectHeader;
use moq_wire::subgroup::SubgroupHeader;
use moq_wire::{
    ErrorCode, KeyValuePairs, Location, NamespaceTrackname, Parameters, RequestId, TrackAlias,
    Version,
};
use quiche_utils::stream_id::StreamID;
use quiche_webtransport as wt;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::time::Duration;

pub use moq_core::session::{
    NamespacePublishRequestAction, PollResult, PublishRequestAction, PublishStatus,
    SubscriptionRequestAction,
};

pub struct MoqTransportSession {
    pub(crate) inner: moq_core::MoqTransportSession,
    pub(crate) webtransport_session_id: StreamID,
}

#[quiche_moq_macros::generate_moq_handle(quiche_moq)]
impl MoqTransportSession {
    /// Control stream is opened and the setup message has been exchanged.
    pub fn initialized(&self) -> bool {
        self.inner.initialized()
    }

    /// Returns the negotiated MoQ version, or `None` if the handshake is not yet complete.
    pub fn version(&self) -> Option<Version> {
        self.inner.version()
    }

    /// Connect to server.
    pub fn connect(
        session_id: StreamID,
        h3_conn: &mut h3::Connection,
        quich_conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        config: Config,
    ) -> Result<MoqTransportSession> {
        let mut t = QuicheTransport::new(session_id.into(), wt, h3_conn, quich_conn);
        let inner = moq_core::MoqTransportSession::connect(&mut t, config)?;
        Ok(Self { inner, webtransport_session_id: session_id })
    }

    /// Accept client (server side).
    pub fn accept(session_id: StreamID, config: Config) -> Result<MoqTransportSession> {
        let inner = moq_core::MoqTransportSession::accept(config)?;
        Ok(Self { inner, webtransport_session_id: session_id })
    }

    /// Subscribes to a track. Returns the request ID.
    /// `h3` is required because draft 17+ opens a dedicated bidi stream per SUBSCRIBE
    /// (`open_stream`), which older drafts (which just queue onto the control stream)
    /// don't need.
    pub fn subscribe(
        &mut self,
        namespace_trackname: &NamespaceTrackname,
        subscriber_priority: Option<u8>,
        delivery_timeout: Option<u64>,
        filter_type: FilterType,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<RequestId> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner
            .subscribe(namespace_trackname, subscriber_priority, delivery_timeout, filter_type, &mut t)
            .map_err(Into::into)
    }

    pub fn request_update_delivery_timeout(
        &mut self,
        existing_request_id: RequestId,
        delivery_timeout: Option<u64>,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner
            .request_update_delivery_timeout(existing_request_id, delivery_timeout, &mut t)
            .map_err(Into::into)
    }

    /// Reconcile MoQ state from the lower layers.
    pub fn handle(
        &mut self,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> PollResult {
        let session_id: u64 = self.webtransport_session_id.into();
        let readable: Vec<u64> = wt.readable_streams(session_id).to_vec();
        let mut t = QuicheTransport::new(session_id, wt, h3, quic);
        self.inner.handle(&mut t, &readable)
    }

    /// Must be called after upper layer work.
    pub fn post_handle(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut wt::Connection,
    ) {
        let session_id: u64 = self.webtransport_session_id.into();
        let mut t = QuicheTransport::new(session_id, wt, h3, quic);
        self.inner.post_handle(&mut t)
    }

    /// Readable track aliases.
    pub fn readable(&self) -> SmallVec<TrackAlias, 8> {
        self.inner.readable()
    }

    /// Writable track aliases.
    pub fn writable(&self) -> SmallVec<TrackAlias, 8> {
        self.inner.writable()
    }

    pub fn send_obj(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner.send_obj(buf, track_alias, &mut t).map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_obj_with(
        &mut self,
        buf: &[u8],
        group_id: Option<u64>,
        object_id: Option<u64>,
        extension_headers: &KeyValuePairs,
        track_alias: TrackAlias,
        received_at: Option<std::time::Instant>,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner
            .send_obj_with(buf, group_id, object_id, extension_headers, track_alias, received_at, &mut t)
            .map_err(Into::into)
    }

    pub fn send_obj_hdr(
        &mut self,
        size: usize,
        track_alias: TrackAlias,
        received_at: Option<std::time::Instant>,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner
            .send_obj_hdr(size, track_alias, received_at, &mut t)
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_obj_hdr_with(
        &mut self,
        group_id: Option<u64>,
        subgroup_id: Option<u64>,
        object_id: Option<u64>,
        size: usize,
        extension_headers: &KeyValuePairs,
        track_alias: TrackAlias,
        received_at: Option<std::time::Instant>,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner
            .send_obj_hdr_with(
                group_id,
                subgroup_id,
                object_id,
                size,
                extension_headers,
                track_alias,
                received_at,
                &mut t,
            )
            .map_err(Into::into)
    }

    pub fn send_obj_pld(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.send_obj_pld(buf, track_alias, &mut t).map_err(Into::into)
    }

    pub fn reset_current_track_stream(
        &mut self,
        track_alias: TrackAlias,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::quic_only(quic);
        self.inner.reset_current_track_stream(track_alias, &mut t)
    }

    pub fn set_out_track_priority(
        &mut self,
        track_alias: TrackAlias,
        urgency: u8,
        incremental: bool,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::quic_only(quic);
        self.inner.set_out_track_priority(track_alias, urgency, incremental, &mut t)
    }

    pub fn pending_received_subscriptions(&self) -> &HashMap<RequestId, SubscribeMessage> {
        self.inner.pending_received_subscriptions()
    }

    pub fn subscription_delivery_timeout(&self, track_alias: TrackAlias) -> Option<Duration> {
        self.inner.subscription_delivery_timeout(track_alias)
    }

    pub fn process_subscription_requests<F>(
        &mut self,
        f: F,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) where
        F: FnMut(&RequestId, &SubscribeMessage, TrackAlias) -> SubscriptionRequestAction,
    {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.process_subscription_requests(f, &mut t)
    }

    pub fn accept_subscription(
        &mut self,
        request_id: RequestId,
        largest_location: Option<Location>,
        max_cache_duration: Option<u64>,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) -> TrackAlias {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.accept_subscription(request_id, largest_location, max_cache_duration, &mut t)
    }

    pub fn reject_subscription(
        &mut self,
        request_id: RequestId,
        error_code: u64,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.reject_subscription(request_id, error_code, &mut t)
    }

    /// `h3` is required because draft 17+ opens a dedicated bidi stream per PUBLISH
    /// (`open_stream`), which older drafts (which just queue onto the control stream)
    /// don't need.
    pub fn publish_track(
        &mut self,
        namespace_trackname: NamespaceTrackname,
        parameters: Parameters,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<(RequestId, TrackAlias)> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner.publish_track(namespace_trackname, parameters, &mut t).map_err(Into::into)
    }

    pub fn poll_publish_response(
        &mut self,
        request_id: RequestId,
    ) -> Option<core::result::Result<PublishOkMessage, RequestErrorMessage>> {
        self.inner.poll_publish_response(request_id)
    }

    pub fn pending_received_track_publishes(&self) -> &HashMap<RequestId, PublishMessage> {
        self.inner.pending_received_track_publishes()
    }

    pub fn process_track_publish_requests<F>(
        &mut self,
        f: F,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) where
        F: FnMut(&RequestId, &PublishMessage) -> PublishRequestAction,
    {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.process_track_publish_requests(f, &mut t)
    }

    pub fn accept_track_publish(
        &mut self,
        request_id: RequestId,
        parameters: Parameters,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.accept_track_publish(request_id, parameters, &mut t)
    }

    pub fn reject_track_publish(
        &mut self,
        request_id: RequestId,
        error_code: ErrorCode,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.reject_track_publish(request_id, error_code, &mut t)
    }

    pub fn pending_received_namespace_publishes(&self) -> &HashMap<RequestId, PublishNamespaceMessage> {
        self.inner.pending_received_namespace_publishes()
    }

    pub fn process_namespace_publish_requests<F>(
        &mut self,
        f: F,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) where
        F: FnMut(&RequestId, &PublishNamespaceMessage) -> NamespacePublishRequestAction,
    {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.process_namespace_publish_requests(f, &mut t)
    }

    pub fn accept_namespace_publish(
        &mut self,
        request_id: RequestId,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.accept_namespace_publish(request_id, &mut t)
    }

    pub fn reject_namespace_publish(
        &mut self,
        request_id: RequestId,
        error_code: ErrorCode,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.reject_namespace_publish(request_id, error_code, &mut t)
    }

    pub fn remaining_object_payload(&self, track_alias: TrackAlias) -> Result<usize> {
        self.inner.remaining_object_payload(track_alias).map_err(Into::into)
    }

    pub fn readable_streams(&self, track_alias: TrackAlias) -> &[u64] {
        self.inner.readable_streams(track_alias)
    }

    pub fn subgroup_header(&self, track_alias: TrackAlias) -> Option<&SubgroupHeader> {
        self.inner.subgroup_header(track_alias)
    }

    pub fn read_obj_hdr(
        &mut self,
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<ObjectHeader> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner.read_obj_hdr(track_alias, &mut t).map_err(Into::into)
    }

    pub fn read_obj_pld(
        &mut self,
        buf: &mut [u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner.read_obj_pld(buf, track_alias, &mut t).map_err(Into::into)
    }

    pub fn cancel_current_in_stream(
        &mut self,
        track_alias: TrackAlias,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::quic_only(quic);
        self.inner.cancel_current_in_stream(track_alias, &mut t)
    }

    pub fn timeout_stream(
        &mut self,
        track_alias: TrackAlias,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::quic_only(quic);
        self.inner.timeout_stream(track_alias, &mut t)
    }

    pub fn poll_subscribe_response(
        &mut self,
        request_id: RequestId,
    ) -> Option<core::result::Result<(TrackAlias, SubscribeOkMessage), RequestErrorMessage>> {
        self.inner.poll_subscribe_response(request_id)
    }

    /// `h3` is required because draft 17+ opens a dedicated bidi stream per
    /// PUBLISH_NAMESPACE (`open_stream`), which older drafts (which just queue onto
    /// the control stream) don't need.
    pub fn publish_namespace(
        &mut self,
        namespace: Vec<Vec<u8>>,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<RequestId> {
        let mut t = QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic);
        self.inner.publish_namespace(namespace, &mut t).map_err(Into::into)
    }

    pub fn publish_done(
        &mut self,
        request_id: RequestId,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.publish_done(request_id, &mut t)
    }

    pub fn publish_namespace_done(
        &mut self,
        request_id: RequestId,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let mut t = QuicheTransport::without_h3(self.webtransport_session_id.into(), wt, quic);
        self.inner.publish_namespace_done(request_id, &mut t)
    }

    pub fn publish_namespace_status(&self, request_id: RequestId) -> PublishStatus {
        self.inner.publish_namespace_status(request_id)
    }

    pub fn start_padding_stream(&mut self) {
        self.inner.start_padding_stream()
    }

    pub fn stop_padding_stream(&mut self, quic: &mut quiche::Connection) {
        let mut t = QuicheTransport::quic_only(quic);
        self.inner.stop_padding_stream(&mut t)
    }

    pub fn padding_stream_running(&self) -> bool {
        self.inner.padding_stream_running()
    }
}

impl<'a> crate::moq_handle::MoqHandle<'a> {
    moq_handle_impl!();
}

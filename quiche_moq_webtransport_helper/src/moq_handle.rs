use std::collections::HashMap;
use quiche::h3;
use quiche_moq::{MoqTransportSession, PublishStatus, Result, StreamID, SubscriptionRequestAction};
use quiche_moq::wire::{Location, Namespace, NamespaceTrackname, RequestId, TrackAlias};
use quiche_moq::wire::control_message::{
    PublishNamespaceMessage, RequestErrorMessage, SubscribeMessage, SubscribeOkMessage,
};
use quiche_moq::wire::Version;
use quiche_moq::wire::object::ObjectHeader;
use quiche_moq::wire::subgroup::SubgroupHeader;
use quiche_webtransport as wt;
use smallvec::SmallVec;

/// Temporary handle that bundles all connection references for ergonomic API calls.
/// Created by calling `MoqWebTransportHelper::moq_handle()`.
pub struct MoqHandle<'a> {
    pub session: &'a mut MoqTransportSession,
    pub(crate) quic: &'a mut quiche::Connection,
    pub(crate) h3: &'a mut h3::Connection,
    pub(crate) wt: &'a mut wt::Connection,
}

impl<'a> MoqHandle<'a> {
    quiche_moq::moq_handle_impl!();

    pub fn quic(&mut self) -> &mut quiche::Connection{
        self.quic
    }
}

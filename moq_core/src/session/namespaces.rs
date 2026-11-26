//! Namespaces we announce: PUBLISH_NAMESPACE and its end, and NAMESPACE on a peer's SUBSCRIBE_NAMESPACE.

use super::*;
use moq_wire::control_message::{NamespaceDoneMessage, NamespaceMessage, PublishBlockedMessage};
use moq_wire::NamespaceTuple;

impl<Q: DropQueue> MoqTransportSession<Q> {
    pub fn publish_namespace(
        &mut self,
        namespace: Namespace,
        t: &mut impl Transport,
    ) -> Result<Announcement<Q>> {
        // draft 07–10: ANNOUNCE has no ID on the wire, isn't limited by
        // MAX_SUBSCRIBE_ID and mustn't make SUBSCRIBE IDs skip
        let pre_request_ids = self.effective_version() < MOQ_VERSION_DRAFT_11;
        self.ensure_open()?;
        let request_id = if pre_request_ids { self.local_request_id() } else { self.available_request_id()? };
        let cm = ControlMessageEnum::PublishNamespace(PublishNamespaceMessage::new(Some(request_id), namespace));
        self.send_request(request_id, &cm, t)?;
        let ControlMessageEnum::PublishNamespace(cm) = cm else { unreachable!() };
        self.pending_sent_publish_namespace.insert(request_id, cm);
        if !pre_request_ids {
            self.next_request_id = self.next_request_id.next();
        }
        Ok(Announcement { obligation: self.obligation(DroppedKind::Announcement, request_id) })
    }

    /// A local ID for a request without one on the wire: ANNOUNCE before draft 11, SUBSCRIBE_UPDATE before draft 14.
    /// - from the top of the ID space, apart from the wire's
    pub(super) fn local_request_id(&mut self) -> RequestId {
        self.local_request_ids += 1;
        RequestId(u64::MAX - self.local_request_ids)
    }

    pub fn publish_namespace_done(&mut self, mut announcement: Announcement<Q>, t: &mut impl Transport) {
        let request_id = self.resolve(&mut announcement.obligation);
        self.end_announcement(request_id, t);
    }

    /// Accepted or still pending.
    pub(super) fn end_announcement(&mut self, request_id: RequestId, t: &mut impl Transport) {
        let namespace = match self.sent_namespaces.remove(&request_id) {
            Some(namespace) => namespace,
            None => match self.pending_sent_publish_namespace.remove(&request_id) {
                Some(cm) => cm.take_track_namespace(),
                None => return,
            },
        };
        if self.control_channel.uses_per_request_streams() {
            // withdrawn by cancelling the request (draft-ietf-moq-transport-19 §6.2)
            self.cancel_request_stream(request_id, t);
        } else {
            self.send_control_message(t, &ControlMessageEnum::PublishNamespaceDone(PublishNamespaceDoneMessage::new(request_id, namespace)));
        }
    }

    /// REQUEST_OK to a SUBSCRIBE_NAMESPACE or SUBSCRIBE_TRACKS: NAMESPACE, NAMESPACE_DONE or PUBLISH_BLOCKED
    /// follow on its stream (draft-ietf-moq-transport-18 §10.18, §10.19).
    fn accept_prefix_subscription(&mut self, request_id: RequestId, t: &mut impl Transport) -> Obligation<Q> {
        self.send_response(request_id, &ControlMessageEnum::RequestOk(RequestOkMessage::new(request_id)), t);
        self.prefix_subscriptions.insert(request_id);
        self.obligation(DroppedKind::PrefixSubscription, request_id)
    }

    pub(crate) fn accept_subscribe_namespace(&mut self, request_id: RequestId, t: &mut impl Transport) -> NamespaceSubscription<Q> {
        NamespaceSubscription { obligation: self.accept_prefix_subscription(request_id, t) }
    }

    pub(crate) fn accept_subscribe_tracks(&mut self, request_id: RequestId, t: &mut impl Transport) -> TracksSubscription<Q> {
        TracksSubscription { obligation: self.accept_prefix_subscription(request_id, t) }
    }

    /// NAMESPACE: the namespace under the subscription's prefix with fields `suffix` after it is published.
    pub fn send_namespace(&mut self, subscription: &NamespaceSubscription<Q>, suffix: NamespaceTuple, t: &mut impl Transport) -> Result<()> {
        self.send_on_prefix_subscription(subscription.request_id(), &ControlMessageEnum::Namespace(NamespaceMessage { suffix }), t)
    }

    /// NAMESPACE_DONE: a namespace `send_namespace` named is no longer published.
    pub fn send_namespace_done(&mut self, subscription: &NamespaceSubscription<Q>, suffix: NamespaceTuple, t: &mut impl Transport) -> Result<()> {
        self.send_on_prefix_subscription(subscription.request_id(), &ControlMessageEnum::NamespaceDone(NamespaceDoneMessage { suffix }), t)
    }

    /// PUBLISH_BLOCKED: the track with `suffix` after the subscription's prefix and `track_name` gets
    /// no PUBLISH, none may follow (draft-ietf-moq-transport-18 §6.1).
    pub fn send_publish_blocked(&mut self, subscription: &TracksSubscription<Q>, suffix: NamespaceTuple, track_name: Vec<u8>, t: &mut impl Transport) -> Result<()> {
        self.send_on_prefix_subscription(subscription.request_id(), &ControlMessageEnum::PublishBlocked(PublishBlockedMessage { suffix, track_name }), t)
    }

    fn send_on_prefix_subscription(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) -> Result<()> {
        self.ensure_open()?;
        if !self.prefix_subscriptions.contains(&request_id) {
            return Err(Error::RequestCancelled);
        }
        self.send_response(request_id, cm, t);
        Ok(())
    }

    /// Ends the subscription by its stream's FIN, for the subscriber every namespace it named is done (§10.18).
    pub fn end_namespace_subscription(&mut self, mut subscription: NamespaceSubscription<Q>, t: &mut impl Transport) {
        let request_id = self.resolve(&mut subscription.obligation);
        self.end_prefix_subscription_stream(request_id, t);
    }

    /// Ends the subscription by its stream's FIN, the PUBLISHes it caused stay.
    pub fn end_tracks_subscription(&mut self, mut subscription: TracksSubscription<Q>, t: &mut impl Transport) {
        let request_id = self.resolve(&mut subscription.obligation);
        self.end_prefix_subscription_stream(request_id, t);
    }

    pub(super) fn end_prefix_subscription_stream(&mut self, request_id: RequestId, t: &mut impl Transport) {
        if self.prefix_subscriptions.remove(&request_id) {
            self.control_channel.end_request_stream(request_id, t);
        }
    }
}

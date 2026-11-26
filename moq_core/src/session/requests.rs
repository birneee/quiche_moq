//! Requests from the peer, answered through `Request` tokens.

use super::*;

impl<Q: DropQueue> MoqTransportSession<Q> {

    /// Fails with `Error::RequestCancelled` if the peer withdrew `request` first.
    ///
    /// Panics if `request` was received by another session.
    pub fn accept_request<K: RequestKind>(&mut self, request: Request<K, Q>, accept: K::Accept, t: &mut impl Transport) -> Result<K::Accepted<Q>> {
        let request_id = request.request_id();
        let (_, message) = self.take_unanswered(request)?;
        Ok(K::accept(self, request_id, message, accept, t, sealed::Token(())))
    }

    /// Answers REQUEST_ERROR. Fails with `Error::RequestCancelled` if the peer
    /// withdrew `request` first.
    ///
    /// Panics if `request` was received by another session.
    pub fn reject_request<K: RequestKind>(&mut self, request: Request<K, Q>, error_code: RequestErrorCode, t: &mut impl Transport) -> Result<()> {
        let request_id = request.request_id();
        let (unanswered, _) = self.take_unanswered(request)?;
        self.send_rejection(request_id, unanswered, error_code, t);
        Ok(())
    }

    fn take_unanswered<K: RequestKind>(&mut self, request: Request<K, Q>) -> Result<(Unanswered, K::Message)> {
        let Request { message, mut obligation } = request;
        let request_id = self.resolve(&mut obligation);
        self.ensure_open()?;
        let unanswered = self.unanswered_requests.remove(&request_id).ok_or(Error::RequestCancelled)?;
        Ok((unanswered, message))
    }

    pub(super) fn obligation(&self, kind: DroppedKind, request_id: RequestId) -> Obligation<Q> {
        Obligation { dropped: Dropped { kind, request_id }, queue: Some(self.dropped.handle()) }
    }

    /// Disarms `obligation`, its value is being resolved.
    ///
    /// Panics if another session created it.
    pub(super) fn resolve(&self, obligation: &mut Obligation<Q>) -> RequestId {
        self.assert_created(obligation);
        obligation.queue = None;
        obligation.request_id()
    }

    pub(super) fn assert_created(&self, obligation: &Obligation<Q>) {
        assert!(
            obligation.queue.as_ref().is_some_and(|queue| self.dropped.owns(queue)),
            "{:?} used on a session that did not create it", obligation.dropped.kind
        );
    }

    pub(super) fn receive_request<K: RequestKind>(
        &mut self,
        request_id: RequestId,
        unanswered: Unanswered,
        message: K::Message,
        incoming: fn(Request<K, Q>) -> IncomingRequest<Q>,
    ) {
        self.unanswered_requests.insert(request_id, unanswered);
        let obligation = self.obligation(DroppedKind::Request, request_id);
        self.events.push_back(Event::Request(incoming(Request { message, obligation })));
    }

    /// The peer ended a subscription to us or its namespace, or withdrew `request_id` before it was answered:
    /// UNSUBSCRIBE, PUBLISH_NAMESPACE_DONE, or from draft 17 cancelling the request.
    pub(super) fn end_peer_request(&mut self, request_id: RequestId, t: &mut impl Transport) {
        if let Some(track_alias) = self.out_requests.remove(&request_id) {
            self.remove_out_track(track_alias, t);
            self.events.push_back(Event::Unsubscribed { request_id, track_alias });
        } else if self.reset_fetch_response(request_id, t) {
            self.events.push_back(Event::FetchCancelled { request_id });
        } else if let Some(namespace) = self.received_namespaces.remove(&request_id) {
            self.events.push_back(Event::NamespaceWithdrawn { request_id, namespace });
        } else if self.prefix_subscriptions.remove(&request_id) {
            self.events.push_back(Event::PrefixSubscriptionEnded { request_id });
        } else {
            self.cancel_request(request_id, t);
        }
    }

    /// draft 17+: the peer reset or stopped `request_id`'s stream, which cancels it (draft-ietf-moq-transport-19 §3.3.3).
    pub(super) fn peer_cancelled_request(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.cancel_request_stream(request_id, t);
        if self.fail_pending_request(request_id, RequestFailure::Cancelled, t) {
            return;
        }
        if let Some(track) = self.active_subscriptions.get(&request_id).and_then(|alias| self.in_tracks.get_mut(alias)) {
            // ended by the publisher as by PUBLISH_DONE, without awaiting further streams
            track.mark_done(0, self.now);
        } else if self.sent_namespaces.remove(&request_id).is_some() {
            self.events.push_back(Event::RequestFailed { request_id, failure: RequestFailure::Cancelled });
        } else {
            self.end_peer_request(request_id, t);
        }
    }

    /// Our unanswered `request_id` failed: forgets it and reports `failure`, `false` if it is none.
    pub(super) fn fail_pending_request(&mut self, request_id: RequestId, failure: RequestFailure, t: &mut impl Transport) -> bool {
        if let Some(subscribe) = self.pending_subscribe.remove(&request_id) {
            self.subscription_options.remove(&request_id);
            // streams that already arrived for it won't be read
            if let Some(track_alias) = subscribe.track_alias() {
                self.drop_in_track(track_alias, t);
            }
        } else if let Some(publish) = self.pending_sent_publish.remove(&request_id) {
            self.out_requests.remove(&request_id);
            self.remove_out_track(publish.track_alias, t);
        } else if self.fail_fetch(request_id, t) {
        } else if !self.pending_request_updates.remove(&request_id)
            && !self.pending_track_status.remove(&request_id)
            && self.pending_sent_publish_namespace.remove(&request_id).is_none()
        {
            return false;
        }
        self.events.push_back(Event::RequestFailed { request_id, failure });
        true
    }

    /// draft 17+: cancels `request_id` (draft-ietf-moq-transport-19 §3.3.3), resetting and stopping its stream.
    pub(super) fn cancel_request_stream(&mut self, request_id: RequestId, t: &mut impl Transport) {
        if let Some(stream_id) = self.control_channel.cancel_request_stream(request_id, t) {
            self.stop_in_stream(stream_id, t);
        }
    }

    pub(super) fn cancel_request(&mut self, request_id: RequestId, t: &mut impl Transport) {
        let Some(unanswered) = self.unanswered_requests.remove(&request_id) else { return };
        if let Unanswered::Publish { track_alias } = unanswered {
            self.drop_in_track(track_alias, t);
        }
        let queued = self.events.len();
        self.events.retain(|e| !matches!(e, Event::Request(r) if r.request_id() == request_id));
        if self.events.len() == queued {
            self.events.push_back(Event::RequestCancelled(request_id));
        }
    }

    /// Ends what was dropped unresolved, unless the peer already did.
    pub(super) fn resolve_dropped(&mut self, t: &mut impl Transport) {
        while let Some(Dropped { kind, request_id }) = self.dropped.pop() {
            debug!("{kind:?} {request_id} dropped unresolved, ending it");
            match kind {
                DroppedKind::Request => {
                    let Some(unanswered) = self.unanswered_requests.remove(&request_id) else { continue };
                    self.send_rejection(request_id, unanswered, RequestErrorCode::InternalError, t);
                }
                DroppedKind::Subscription => self.end_subscription(request_id, t),
                DroppedKind::Publication => self.end_publication(request_id, PublishDoneStatus::InternalError, t),
                DroppedKind::Announcement => self.end_announcement(request_id, t),
                DroppedKind::Fetching => self.end_fetching(request_id, t),
                DroppedKind::FetchResponse => {
                    self.reset_fetch_response(request_id, t);
                    self.cancel_request_stream(request_id, t);
                }
                DroppedKind::PrefixSubscription => self.end_prefix_subscription_stream(request_id, t),
            }
        }
    }

    pub(super) fn send_rejection(&mut self, request_id: RequestId, unanswered: Unanswered, error_code: RequestErrorCode, t: &mut impl Transport) {
        let track_alias = match unanswered {
            Unanswered::Subscribe { track_alias } => track_alias,
            Unanswered::Publish { track_alias } => {
                self.drop_in_track(track_alias, t);
                None
            }
            Unanswered::PublishNamespace { namespace } if !self.effective_version().has_generic_replies() => {
                let cm = PublishNamespaceErrorMessage::new(request_id, namespace, error_code);
                self.send_final_response(request_id, &ControlMessageEnum::PublishNamespaceError(cm), t);
                return;
            }
            Unanswered::RequestUpdate { existing_request_id } => {
                let cm = RequestErrorMessage::new(request_id, error_code);
                self.send_response(existing_request_id, &ControlMessageEnum::RequestError(cm), t);
                return;
            }
            Unanswered::TrackStatus { namespace_trackname } if !self.effective_version().has_generic_replies() => {
                let cm = TrackStatusErrorMessage::new(request_id, namespace_trackname, error_code);
                self.send_final_response(request_id, &ControlMessageEnum::TrackStatusError(cm), t);
                return;
            }
            Unanswered::Fetch => {
                let cm = self.fetch_error(request_id, error_code).unwrap_or_else(|| ControlMessageEnum::RequestError(RequestErrorMessage::new(request_id, error_code)));
                self.send_final_response(request_id, &cm, t);
                return;
            }
            Unanswered::PublishNamespace { .. } | Unanswered::TrackStatus { .. } | Unanswered::SubscribeNamespace | Unanswered::SubscribeTracks => None,
        };
        let cm = RequestErrorMessage::new(request_id, error_code).with_track_alias(track_alias);
        self.send_final_response(request_id, &ControlMessageEnum::RequestError(cm), t);
    }

    pub(crate) fn accept_subscribe(&mut self, cm: SubscribeMessage, ok: SubscribeOk, t: &mut impl Transport) -> Publication<Q> {
        let track_alias = match cm.track_alias {
            // the subscriber's until draft 11
            Some(track_alias) => track_alias,
            None => {
                let track_alias = self.next_out_track_alias;
                self.next_out_track_alias.0 += 1;
                track_alias
            }
        };
        let reply = SubscribeOkMessage::new(cm.request_id, track_alias, ok.largest_location, ok.max_cache_duration);
        self.send_response(cm.request_id, &ControlMessageEnum::SubscribeOk(reply), t);
        let timeouts = DeliveryTimeouts::from_millis(cm.object_delivery_timeout, cm.subgroup_delivery_timeout);
        let max_cache_duration = ok.max_cache_duration.map(Duration::from_millis);
        let version = self.effective_version();
        let mut track = OutTrack::new(timeouts, max_cache_duration, version);
        // validated on receipt
        track.object_filter = self.object_filter(&cm.range_filters).unwrap_or_default();
        self.out_tracks.insert(track_alias, track);
        self.out_requests.insert(cm.request_id, track_alias);
        Publication { obligation: self.obligation(DroppedKind::Publication, cm.request_id), track_alias }
    }

    pub(crate) fn accept_publish(&mut self, cm: PublishMessage, ok: crate::PublishOk, t: &mut impl Transport) -> Subscription<Q> {
        let reply = PublishOkMessage {
            forward: ok.forward,
            subscriber_priority: ok.subscriber_priority,
            filter: ok.filter,
            object_delivery_timeout: ok.object_delivery_timeout,
            subgroup_delivery_timeout: ok.subgroup_delivery_timeout,
            ..PublishOkMessage::new(cm.request_id)
        };
        self.send_response(cm.request_id, &ControlMessageEnum::PublishOk(reply), t);
        // routes its PUBLISH_DONE
        self.active_subscriptions.insert(cm.request_id, cm.track_alias);
        let filter = ok.filter.resolved(cm.largest_location, self.effective_version());
        let options = SubscribeOptions { priority: ok.subscriber_priority, delivery_timeout: ok.object_delivery_timeout, subgroup_delivery_timeout: ok.subgroup_delivery_timeout, filter, forward: ok.forward, rendezvous_timeout: None };
        self.subscription_options.insert(cm.request_id, options);
        Subscription { obligation: self.obligation(DroppedKind::Subscription, cm.request_id) }
    }

    pub(crate) fn accept_publish_namespace(&mut self, request_id: RequestId, cm: PublishNamespaceMessage, t: &mut impl Transport) {
        // ANNOUNCE_OK, PUBLISH_NAMESPACE_OK (draft 14) and REQUEST_OK (draft 15+) share type 0x7
        let reply = RequestOkMessage::for_publish_namespace(request_id, cm.track_namespace().clone());
        self.send_response(request_id, &ControlMessageEnum::RequestOk(reply), t);
        self.received_namespaces.insert(request_id, cm.take_track_namespace());
    }

    pub(crate) fn accept_track_status(&mut self, request_id: RequestId, cm: TrackStatusMessage, ok: SubscribeOk, t: &mut impl Transport) {
        let track_properties: KeyValuePairs = ok.max_cache_duration.map(|ms| KeyValuePair::Varint(moq_wire::property::MAX_CACHE_DURATION, ms)).into_iter().collect();
        let reply = if self.effective_version().has_generic_replies() {
            ControlMessageEnum::RequestOk(RequestOkMessage::for_track_status(request_id, ok.largest_location, track_properties))
        } else {
            ControlMessageEnum::TrackStatusOk(TrackStatusOkMessage::new(request_id, cm.namespace_trackname, ok.largest_location, track_properties))
        };
        self.send_final_response(request_id, &reply, t);
    }

    pub(crate) fn accept_request_update(&mut self, cm: RequestUpdateMessage, largest_location: Option<Location>, t: &mut impl Transport) {
        // only REQUEST_UPDATE, from draft 15, is answered and has a Request ID then
        let Some(request_id) = cm.request_id else { return };
        let reply = RequestOkMessage::for_request_update(request_id, largest_location);
        self.send_response(cm.existing_request_id, &ControlMessageEnum::RequestOk(reply), t);
    }
}

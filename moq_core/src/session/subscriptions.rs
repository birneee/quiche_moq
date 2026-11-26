//! Our subscriptions: SUBSCRIBE, receiving their objects, ending them.

use super::*;

impl<Q: DropQueue> MoqTransportSession<Q> {
    /// Subscribes to a track, answered by `Event::SubscribeOk` or `Event::RequestFailed`.
    pub fn subscribe(
        &mut self,
        namespace_trackname: &NamespaceTrackname,
        options: SubscribeOptions,
        t: &mut impl Transport,
    ) -> Result<Subscription<Q>> {
        let request_id = self.available_request_id()?;
        // drafts 7–11: the subscriber picks the alias, its request id
        let track_alias = (!self.effective_version().publisher_assigns_track_alias()).then_some(TrackAlias(request_id.0));
        let cm = ControlMessageEnum::Subscribe(SubscribeMessage {
            track_alias,
            subscriber_priority: options.priority,
            group_order: Some(GroupOrder::Descending),
            forward: options.forward,
            filter: options.filter,
            object_delivery_timeout: options.delivery_timeout,
            subgroup_delivery_timeout: options.subgroup_delivery_timeout,
            rendezvous_timeout: options.rendezvous_timeout,
            ..SubscribeMessage::new(request_id, namespace_trackname.clone())
        });
        self.send_request(request_id, &cm, t)?;
        self.pending_subscribe.insert(request_id, PendingSubscribe::new(track_alias));
        self.subscription_options.insert(request_id, options);
        self.next_request_id = self.next_request_id.next();
        debug!("moq subscribe {}", &namespace_trackname);
        Ok(Subscription { obligation: self.obligation(DroppedKind::Subscription, request_id) })
    }

    /// Asks for a track's status, answered by `Event::TrackStatus` or `Event::RequestFailed`.
    // TODO before draft 13, whose reply is a Status Code that a failure isn't told from by its type
    pub fn track_status(&mut self, namespace_trackname: &NamespaceTrackname, t: &mut impl Transport) -> Result<RequestId> {
        if self.effective_version().draft() < 13 {
            return Err(Error::Unimplemented);
        }
        let request_id = self.available_request_id()?;
        let cm = TrackStatusMessage { request_id: Some(request_id), namespace_trackname: namespace_trackname.clone() };
        self.send_request(request_id, &ControlMessageEnum::TrackStatus(cm), t)?;
        self.pending_track_status.insert(request_id);
        self.next_request_id = self.next_request_id.next();
        Ok(request_id)
    }

    /// Ends a subscription and forgets its state, also after `recv_track_fully_done`.
    /// - sends UNSUBSCRIBE (draft-16 §9.12) unless the publisher already ended it
    ///   with PUBLISH_DONE or rejected it
    pub fn unsubscribe(&mut self, mut subscription: Subscription<Q>, t: &mut impl Transport) {
        let request_id = self.resolve(&mut subscription.obligation);
        self.end_subscription(request_id, t);
    }

    pub(super) fn end_subscription(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.subscription_options.remove(&request_id);
        let pending = self.pending_subscribe.remove(&request_id);
        let active_alias = self.active_subscriptions.remove(&request_id);
        let publisher_ended = active_alias
            .and_then(|alias| self.in_tracks.get(&alias))
            .is_some_and(InTrack::publish_done_received);
        let publisher_sending = pending.is_some() || active_alias.is_some() && !publisher_ended;
        if self.control_channel.uses_per_request_streams() {
            // cancelling the request replaces UNSUBSCRIBE (draft-ietf-moq-transport-19 §5.1)
            if publisher_sending {
                self.cancel_request_stream(request_id, t);
            } else {
                self.control_channel.end_request_stream(request_id, t);
            }
        } else if publisher_sending {
            self.send_control_message(
                t,
                &ControlMessageEnum::Unsubscribe(UnsubscribeMessage { request_id }),
            );
        }
        if let Some(track_alias) = active_alias.or_else(|| pending.and_then(|p| p.track_alias())) {
            self.drop_in_track(track_alias, t);
        }
        // a response already queued is stale now
        self.events.retain(|e| !matches!(e, Event::SubscribeOk { request_id: id, .. } | Event::RequestFailed { request_id: id, .. } if *id == request_id));
    }

    /// Send a REQUEST_UPDATE to change the DELIVERY_TIMEOUT on an existing upstream subscription.
    /// It can't remove one: an update without the parameter keeps it.
    pub fn request_update_delivery_timeout(
        &mut self,
        subscription: &Subscription<Q>,
        delivery_timeout: u64,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.send_request_update(subscription, |update| update.object_delivery_timeout = Some(delivery_timeout), t)?;
        Ok(())
    }

    /// REQUEST_UPDATE of the FORWARD State (draft-16 §9.2.2.8), optionally
    /// re-anchoring the SUBSCRIPTION_FILTER (§9.2.2.5). Returns the update's
    /// own Request ID, answered by `Event::RequestUpdateOk` or `Event::RequestFailed`.
    /// - SUBSCRIBE_UPDATE before draft 15 has no reply, `Event::RequestUpdateOk` without LARGEST_OBJECT follows once sent
    pub fn request_update_forward(
        &mut self,
        subscription: &Subscription<Q>,
        forward: bool,
        filter: Option<LocationFilter>,
        t: &mut impl Transport,
    ) -> Result<RequestId> {
        let request_id = self.send_request_update(subscription, |update| {
            update.forward = Some(forward);
            update.filter = filter;
        }, t)?;
        if self.effective_version().request_update_has_parameters() {
            self.pending_request_updates.insert(request_id);
        } else {
            self.events.push_back(Event::RequestUpdateOk { request_id, largest_location: None });
        }
        Ok(request_id)
    }

    /// Sends a REQUEST_UPDATE of `subscription` that `change` fills in, returns its Request ID, a local one
    /// before draft 14. Before draft 15 SUBSCRIBE_UPDATE repeats the whole subscription.
    /// - `Error::Wire` for what `version` can't express, e.g. Forward State 0 before draft 11 or a relative
    ///   filter before draft 15, which could widen the subscription
    fn send_request_update(&mut self, subscription: &Subscription<Q>, change: impl FnOnce(&mut RequestUpdateMessage), t: &mut impl Transport) -> Result<RequestId> {
        self.assert_created(&subscription.obligation);
        let version = self.effective_version();
        let existing_request_id = subscription.request_id();
        let mut options = *self.subscription_options.get(&existing_request_id).ok_or(Error::TrackEnded)?;
        let wire_request_id = version.request_update_has_request_id().then(|| self.available_request_id()).transpose()?;
        let mut update = RequestUpdateMessage::new(wire_request_id, existing_request_id);
        change(&mut update);
        options.priority = update.subscriber_priority.unwrap_or(options.priority);
        options.forward = update.forward.unwrap_or(options.forward);
        options.filter = update.filter.unwrap_or(options.filter);
        options.delivery_timeout = update.object_delivery_timeout.or(options.delivery_timeout);
        options.subgroup_delivery_timeout = update.subgroup_delivery_timeout.or(options.subgroup_delivery_timeout);
        if !version.request_update_has_parameters() {
            update.subscriber_priority = Some(options.priority);
            update.forward = Some(options.forward);
            update.filter = Some(options.filter);
            update.object_delivery_timeout = options.delivery_timeout;
        }
        let request_id = wire_request_id.unwrap_or_else(|| self.local_request_id());
        match self.control_channel.send_update(existing_request_id, request_id, &ControlMessageEnum::RequestUpdate(update), version, t) {
            Ok(()) => {}
            // encoded before anything is sent
            Err(e @ (Error::TrackEnded | Error::RequestBlocked | Error::Wire(_))) => return Err(e),
            Err(e) => {
                self.control_stream_failed(e, t);
                return Err(Error::SessionClosed);
            }
        }
        self.subscription_options.insert(existing_request_id, options);
        if wire_request_id.is_some() {
            self.next_request_id = self.next_request_id.next();
        }
        Ok(request_id)
    }

    /// Stops the streams that waited `STRAY_STREAM_TIMEOUT` for a track and
    /// forgets aliases dropped as long ago.
    pub(super) fn expire_stray_streams(&mut self, now: Timestamp, t: &mut impl Transport) {
        let expired: SmallVec<(TrackAlias, Vec<u64>), 4> = self.pending_streams
            .extract_if(|_, (since, _)| now.saturating_duration_since(*since) >= STRAY_STREAM_TIMEOUT)
            .map(|(alias, (_, streams))| (alias, streams))
            .collect();
        for (alias, streams) in expired {
            debug!("no track for alias {alias}, stopping its streams");
            for stream_id in streams {
                self.in_streams.remove(&stream_id);
                self.stop_in_stream(stream_id, t);
            }
        }
        self.pending_datagrams.retain(|_, (since, _)| now.saturating_duration_since(*since) < STRAY_STREAM_TIMEOUT);
        self.dropped_aliases.retain(|_, at| now.saturating_duration_since(*at) < STRAY_STREAM_TIMEOUT);
    }

    /// Stops reading `stream_id`, drained until it ends unless the transport discards it.
    pub(super) fn stop_in_stream(&mut self, stream_id: u64, t: &mut impl Transport) {
        if !t.stop_sending(stream_id, ResetCode::Canceled) {
            self.dropped_in_streams.insert(stream_id);
        }
    }

    /// Readable track aliases.
    pub fn readable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.in_tracks {
            if track.readable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// PUBLISH_DONE arrived and every stream it counted finished: no stream
    /// of the track follows. `Error::Fin` only ends one stream.
    /// - then `unsubscribe` releases the track's state
    pub fn recv_track_fully_done(&self, track_alias: TrackAlias) -> bool {
        self.in_tracks.get(&track_alias).is_some_and(|t| t.is_fully_done(self.now))
    }

    /// Stops reading `track_alias`: its streams get STOP_SENDING, unread they
    /// would hold flow-control credit.
    pub(super) fn drop_in_track(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        self.in_tracks.remove(&track_alias);
        self.dropped_aliases.insert(track_alias, self.now);
        self.pending_streams.remove(&track_alias);
        self.pending_datagrams.remove(&track_alias);
        let streams: SmallVec<u64, 4> = self.in_streams.iter()
            .filter(|(_, s)| s.subgroup_header().is_some_and(|h| h.track_alias() == track_alias))
            .map(|(&stream_id, _)| stream_id)
            .collect();
        for stream_id in streams {
            self.in_streams.remove(&stream_id);
            self.stop_in_stream(stream_id, t);
        }
    }

    /// Reads and throws away what arrives on a dropped stream, forgets it once it ends.
    pub(super) fn discard_dropped_stream(&mut self, stream_id: u64, t: &mut impl Transport) {
        let mut scratch = [0u8; 4096];
        loop {
            match t.recv_stream(stream_id, &mut scratch) {
                Ok(0) | Err(StreamError::Done) => return,
                Ok(_) => {}
                Err(_) => {
                    self.dropped_in_streams.remove(&stream_id);
                    return;
                }
            }
        }
    }

    /// Removes `stream_id` from `in_streams` and retires it from
    /// `track_alias`'s bookkeeping (`InTrack::fin_stream`).
    fn retire_in_stream(&mut self, track_alias: TrackAlias, stream_id: u64) {
        self.in_streams.remove(&stream_id);
        if let Some(track) = self.in_tracks.get_mut(&track_alias) {
            track.fin_stream(stream_id);
        }
    }

    /// Reads and throws away whatever payload has arrived on the streams in
    /// `duplicate_discard` (see `recv_new_obj_hdr`), so they don't stall the
    /// objects that follow on them.
    fn drain_duplicates(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        let Some(track) = self.in_tracks.get(&track_alias) else { return };
        let streams: SmallVec<u64, 4> = track.duplicate_discard.keys().copied().collect();
        for stream_id in streams {
            let Some(track) = self.in_tracks.get_mut(&track_alias) else { return };
            let Some(&remaining) = track.duplicate_discard.get(&stream_id) else { continue };
            let Some(stream) = self.in_streams.get_mut(&stream_id) else { continue };
            match stream.discard_obj_pld(t, remaining) {
                Ok(n) if n == remaining => { track.duplicate_discard.remove(&stream_id); }
                Ok(n) => { track.duplicate_discard.insert(stream_id, remaining - n); }
                Err(Error::Fin) => self.retire_in_stream(track_alias, stream_id),
                Err(_) => {}
            }
        }
    }

    /// The next object header on `track_alias`, from any of its streams, or
    /// the end of a Subgroup. Call until `Done`/`Fin`, several streams may
    /// have data.
    /// - a relay passes the Subgroup ID on to `send_obj_hdr`, it sets the
    ///   send priority (draft-19 §7)
    ///
    /// A second stream delivering an Object already returned (a Subgroup
    /// moved to a new stream after a reset, draft-16 §2.2) isn't surfaced
    /// again. It must match `subgroup_id`/`payload_len` (§9.1), then:
    /// - owner still mid-object: kept as standby, takes over if the owner is
    ///   reset (`InTrack::standby`)
    /// - else: read and discarded (`drain_duplicates`)
    ///
    /// A mismatch is a malformed track (§9.1/§2.4.2): the stream is stopped,
    /// `Error::MalformedTrack` returned, and the caller must cancel the
    /// subscription (relays: PUBLISH_DONE/MALFORMED_TRACK downstream).
    pub fn recv_new_obj_hdr(
        &mut self,
        track_alias: TrackAlias,
        t: &mut impl Transport,
    ) -> Result<RecvObjHdr> {
        // streams found empty during this call
        let mut exhausted: SmallVec<u64, 4> = SmallVec::new();
        if let Some(datagram) = self.in_tracks.get_mut(&track_alias).and_then(InTrack::pop_datagram) {
            return Ok(datagram);
        }
        loop {
            self.drain_duplicates(track_alias, t);

            let Some(track) = self.in_tracks.get(&track_alias) else { return Err(Error::Done) };
            // Ready for a header read if unclaimed (no object read yet) or
            // its current object is fully read (a Subgroup can pack several)
            // — and not internally discarded or already found empty.
            let ready = track.readable_streams().iter().copied().find(|sid| {
                !exhausted.contains(sid)
                    && !track.duplicate_discard.contains_key(sid)
                    && self.in_streams.get(sid).is_some_and(|s| {
                        s.current_location().is_none() || s.remaining_object_payload() == 0
                    })
            });
            let Some(stream_id) = ready else {
                return if track.is_fully_done(self.now) { Err(Error::Fin) } else { Err(Error::Done) };
            };

            let Some(stream) = self.in_streams.get_mut(&stream_id) else { return Err(Error::Done) };
            let crate::in_stream::ObjectRead { location: loc, subgroup_id, publisher_priority, end_of_group, header: hdr } = match stream.read_obj_hdr(t) {
                Ok(read) => read,
                Err(Error::Fin) => {
                    // The stream finished before this read, i.e. cleanly, with no
                    // partial object left hanging — report it if it ever delivered
                    // anything (draft-19 §2.2 also allows a FIN with zero Objects,
                    // to explicitly declare a Subgroup entirely unsent; nothing
                    // useful to report there).
                    let stream = &self.in_streams[&stream_id];
                    let finished = stream.current_location().zip(stream.subgroup_location())
                        .map(|(location, sg)| RecvObjHdr::SubgroupFinished { location, subgroup_id: sg.subgroup_id });
                    self.retire_in_stream(track_alias, stream_id);
                    match finished {
                        Some(event) => return Ok(event),
                        None => continue,
                    }
                }
                Err(Error::Transport(StreamError::StreamReset(_))) => {
                    let stream = &self.in_streams[&stream_id];
                    let reset = stream.current_location().zip(stream.subgroup_location())
                        .map(|(location, sg)| RecvObjHdr::SubgroupReset { location, subgroup_id: sg.subgroup_id });
                    self.retire_in_stream(track_alias, stream_id);
                    match reset {
                        Some(event) => return Ok(event),
                        None => continue,
                    }
                }
                // nothing more yet: try the next stream, subgroups must not block each other
                Err(Error::Done) => {
                    exhausted.push(stream_id);
                    continue;
                }
                Err(Error::Wire(e)) => {
                    let _ = self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed object header: {e:?}"), t);
                    return Err(Error::Wire(e));
                }
                Err(e) => return Err(e),
            };
            let Some(track) = self.in_tracks.get_mut(&track_alias) else { return Err(Error::Done) };
            // the stream moved on: its previous object is no longer readable
            track.owned_locations.retain(|_, o| o.stream_id != stream_id);
            match track.owned_locations.get(&loc) {
                None => {
                    track.owned_locations.insert(
                        loc,
                        crate::in_track::OwnedLocation { stream_id, subgroup_id, payload_len: hdr.payload_len() },
                    );
                    return Ok(RecvObjHdr::Header { location: loc, subgroup_id, publisher_priority, end_of_group, header: hdr });
                }
                Some(owner) if owner.subgroup_id == subgroup_id && owner.payload_len == hdr.payload_len() => {
                    // Consistent duplicate — not surfaced; loop and try again.
                    let owner_incomplete =
                        self.in_streams.get(&owner.stream_id).is_some_and(|s| s.remaining_object_payload() > 0);
                    if owner_incomplete {
                        track.standby.insert(stream_id, loc);
                    } else if hdr.payload_len() > 0 {
                        track.duplicate_discard.insert(stream_id, hdr.payload_len());
                    }
                }
                Some(_) => {
                    self.stop_in_stream(stream_id, t);
                    self.retire_in_stream(track_alias, stream_id);
                    return Err(Error::MalformedTrack);
                }
            }
        }
    }

    /// Reads more payload bytes for the object at `location` on
    /// `track_alias`, if a header for it has already been returned by
    /// `recv_new_obj_hdr` and its owning stream has more to deliver.
    /// `Error::Done` if nothing new is available right now — same as if
    /// `location` isn't a currently-owned object at all.
    pub fn recv_obj_pld(
        &mut self,
        track_alias: TrackAlias,
        location: Location,
        buf: &mut [u8],
        t: &mut impl Transport,
    ) -> Result<usize> {
        loop {
            let Some(track) = self.in_tracks.get(&track_alias) else { return Err(Error::Done) };
            let Some(owned) = track.owned_locations.get(&location) else { return Err(Error::Done) };
            let (stream_id, payload_len) = (owned.stream_id, owned.payload_len);
            let read = match self.skip_handover_prefix(track_alias, stream_id, t) {
                Ok(()) => match self.in_streams.get_mut(&stream_id) {
                    Some(stream) => stream.read_obj_pld(t, buf),
                    None => return Err(Error::Done),
                },
                Err(e) => Err(e),
            };
            match read {
                Ok(n) => {
                    if self.in_streams.get(&stream_id).is_some_and(|s| s.remaining_object_payload() == 0)
                        && let Some(track) = self.in_tracks.get_mut(&track_alias)
                    {
                        track.discard_standby(location, payload_len);
                    }
                    return Ok(n);
                }
                // The owner ended mid-object (reset, or a truncating FIN): a
                // standby duplicate takes over.
                Err(Error::Fin) => {
                    let delivered = self.obj_pld_received(track_alias, location).unwrap_or(0);
                    let handed_over = self.in_tracks.get_mut(&track_alias).and_then(|track| track.hand_over(location, delivered));
                    self.retire_in_stream(track_alias, stream_id);
                    if handed_over.is_none() {
                        return Err(Error::Fin);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Drops the prefix `stream_id` must skip after taking over an object
    /// (`InTrack::handover_skip`). `Ok(())` once nothing is left to skip.
    fn skip_handover_prefix(&mut self, track_alias: TrackAlias, stream_id: u64, t: &mut impl Transport) -> Result<()> {
        let Some(track) = self.in_tracks.get_mut(&track_alias) else { return Err(Error::Done) };
        let Some(&skip) = track.handover_skip.get(&stream_id) else { return Ok(()) };
        let Some(stream) = self.in_streams.get_mut(&stream_id) else { return Err(Error::Done) };
        let n = stream.discard_obj_pld(t, skip)?;
        if n < skip {
            track.handover_skip.insert(stream_id, skip - n);
            return Err(Error::Done);
        }
        track.handover_skip.remove(&stream_id);
        Ok(())
    }

    /// Bytes of `location`'s payload delivered so far via `recv_obj_pld` —
    /// `None` if its header hasn't been read yet, or the stream's retired.
    /// Valid from `recv_new_obj_hdr` returning the header until that
    /// stream's *next* `recv_new_obj_hdr` call moves on to another object.
    pub fn obj_pld_received(&self, track_alias: TrackAlias, location: Location) -> Option<usize> {
        let track = self.in_tracks.get(&track_alias)?;
        let owned = track.owned_locations.get(&location)?;
        let stream = self.in_streams.get(&owned.stream_id)?;
        let skip = track.handover_skip.get(&owned.stream_id).copied().unwrap_or(0);
        Some(owned.payload_len - stream.remaining_object_payload() + skip)
    }

    /// Stops the stream delivering `location` (STOP_SENDING): later Objects
    /// on it depend on this one (draft-19 §2.2). The Subgroup may continue
    /// on a new stream, which `recv_new_obj_hdr` picks up.
    pub fn cancel_recv_obj(&mut self, track_alias: TrackAlias, location: Location, t: &mut impl Transport) {
        let Some(track) = self.in_tracks.get_mut(&track_alias) else { return };
        let Some(stream_id) = track.owned_locations.get(&location).map(|o| o.stream_id) else { return };
        let standby = track.take_standby(location);
        for stream_id in standby.into_iter().chain([stream_id]) {
            self.stop_in_stream(stream_id, t);
            self.retire_in_stream(track_alias, stream_id);
        }
    }

}

/// How to subscribe, by default as the spec's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscribeOptions {
    pub priority: u8,
    /// OBJECT_DELIVERY_TIMEOUT in milliseconds
    pub delivery_timeout: Option<u64>,
    /// SUBGROUP_DELIVERY_TIMEOUT in milliseconds, draft 18 on
    pub subgroup_delivery_timeout: Option<u64>,
    pub filter: LocationFilter,
    /// The initial Forward State
    pub forward: bool,
    /// RENDEZVOUS_TIMEOUT in milliseconds, how long a relay holds the subscription for a publisher, draft 17 on
    pub rendezvous_timeout: Option<u64>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self { priority: DEFAULT_SUBSCRIBER_PRIORITY, delivery_timeout: None, subgroup_delivery_timeout: None, filter: LocationFilter::LargestObject, forward: true, rendezvous_timeout: None }
    }
}

//! Our egress tracks: PUBLISH, accepted SUBSCRIBEs, sending objects, PUBLISH_DONE.

use super::*;
use moq_wire::range_filter::FilteredObject;
use moq_wire::DEFAULT_PUBLISHER_PRIORITY;
use moq_wire::object::ObjectStatus;

/// An object's header for `send_obj_hdr`, `OutgoingObject::new` defaults the rest.
#[derive(Clone, Copy, Debug)]
pub struct OutgoingObject<'a> {
    pub location: Location,
    /// The original publisher's grouping (draft-19 §11.2.2), a relay keeps
    /// the one it received the object on. Lower ones are scheduled first.
    pub subgroup_id: u64,
    /// The Subgroup's, a relay keeps the original publisher's (draft-ietf-moq-transport-18 §7.3).
    pub publisher_priority: u8,
    /// Payload length.
    pub size: usize,
    /// The Object Status of an object without payload (draft-ietf-moq-transport-18 §11.2.1.1).
    pub status: ObjectStatus,
    /// The Subgroup holds its Group's largest Object: sets END_OF_GROUP
    /// (draft-14+) when a new stream is opened for it.
    pub end_of_group: bool,
    /// Objects right before this one in its Group confirmed to never exist
    /// (draft-19 §12.9), sent as a Prior Object ID Gap property.
    pub prior_object_id_gap: u64,
    pub properties: Option<&'a KeyValuePairs>,
    /// When the object was received, checked against `delivery_timeout`
    /// and `max_cache_duration`.
    pub received_at: Option<Timestamp>,
}

impl OutgoingObject<'_> {
    pub fn new(location: Location, subgroup_id: u64, size: usize) -> Self {
        Self { location, subgroup_id, publisher_priority: DEFAULT_PUBLISHER_PRIORITY, size, status: ObjectStatus::Normal, end_of_group: false, prior_object_id_gap: 0, properties: None, received_at: None }
    }
}

impl<Q: DropQueue> MoqTransportSession<Q> {
    /// Writable track aliases.
    pub fn writable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.out_tracks {
            if track.writable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// Sends a new object's header — on the stream already open for its
    /// (Group, Subgroup), or a new one.
    /// - object ids must increase per Subgroup, a gap needs no confirmation
    ///   unless `prior_object_id_gap` declares it (draft-19 §12.9)
    pub fn send_obj_hdr(&mut self, track_alias: TrackAlias, object: OutgoingObject<'_>, t: &mut impl Transport) -> Result<()> {
        let OutgoingObject { location, subgroup_id, publisher_priority, size, status, end_of_group, prior_object_id_gap, properties, received_at } = object;
        let version = self.effective_version();
        let Self { out_tracks, out_streams, earliest_deadline, now, .. } = self;
        let track = out_tracks.get_mut(&track_alias).filter(|track| !track.ended).ok_or(Error::TrackEnded)?;
        track.check_sendable(received_at, *now)?;
        let Location { group: group_id, object: object_id } = location;

        if prior_object_id_gap > object_id {
            return Err(Error::InvalidGap);
        }
        // draft-19 §12.9: at most one Prior Object ID Gap per object
        if prior_object_id_gap > 0 && properties.is_some_and(|e| e.get_varint(moq_wire::property::PRIOR_OBJECT_ID_GAP).is_some()) {
            return Err(Error::InvalidGap);
        }
        if prior_object_id_gap > 0 && !track.may_declare_gap(group_id, object_id - prior_object_id_gap) {
            return Err(Error::InvalidGap);
        }

        let mut ext = properties.cloned().unwrap_or_default();
        if prior_object_id_gap > 0 {
            ext.insert(KeyValuePair::Varint(moq_wire::property::PRIOR_OBJECT_ID_GAP, prior_object_id_gap));
        }

        if !track.object_filter.passes(&FilteredObject { subgroup_id: Some(subgroup_id), object_id, publisher_priority, properties: &ext }) {
            return Err(Error::ObjectFiltered);
        }

        let sg = SubgroupLocation { group_id, subgroup_id };
        let open = track.open_streams.get(&sg).map_or(&[][..], Vec::as_slice);
        if open.iter().any(|sid| out_streams.get(sid).is_some_and(|s| s.fin_sent())) {
            return Err(Error::SubgroupFinished);
        }
        // The open stream of this Subgroup whose tail is closest below
        // `object_id`: out-of-order upstream delivery can need several
        // (draft-19 §2.2), object ids only increase per stream.
        let continuing = open.iter()
            .filter_map(|&sid| Some((sid, out_streams.get(&sid)?.next_object_id())))
            .filter(|&(_, next)| next <= object_id)
            .max_by_key(|&(_, next)| next)
            .map(|(sid, _)| sid);

        let stream = match continuing.and_then(|sid| out_streams.get_mut(&sid)) {
            Some(stream) => stream,
            None => {
                let stream_id = match t.open_uni_stream() {
                    Ok(s) => s,
                    // stream limit reached, retry later
                    Err(StreamError::Done) => return Err(Error::Done),
                    Err(e) => return Err(e.into()),
                };
                track.open_streams.entry(sg).or_default().push(stream_id);
                track.streams_opened += 1;
                t.set_stream_priority(stream_id, track.urgency, track.incremental);
                out_streams.entry(stream_id).insert(OutStream::new(stream_id, track_alias, group_id, subgroup_id, publisher_priority, version, end_of_group)).into_mut()
            }
        };
        stream.send_obj_hdr(Some(object_id), size, status, &ext, received_at, t)?;
        track.note_sent(group_id, object_id);
        if let Some(deadline) = received_at.and_then(|at| track.expiry_deadline(at)) {
            *earliest_deadline = Some(earliest_deadline.map_or(deadline, |e| e.min(deadline)));
        }
        Ok(())
    }

    /// The open stream currently mid-payload for `location` on `track_alias`,
    /// if any — at most one can be, since an object_id is only ever header'd
    /// on one stream. Shared by `send_obj_pld`/`cancel_send_obj`.
    fn stream_sending(&self, track_alias: TrackAlias, location: Location) -> Option<u64> {
        let track = self.out_tracks.get(&track_alias)?;
        track.open_streams.iter()
            .filter(|(sg, _)| sg.group_id == location.group)
            .flat_map(|(_, stream_ids)| stream_ids.iter().copied())
            .find(|sid| self.out_streams.get(sid).and_then(|s| s.current_object_id()) == Some(location.object))
    }

    /// Writes payload bytes for the object at `location` on `track_alias`.
    /// Returns `Err(SubgroupNotOpen)` if no stream is currently sending it.
    pub fn send_obj_pld(
        &mut self,
        track_alias: TrackAlias,
        location: Location,
        buf: &[u8],
        t: &mut impl Transport,
    ) -> Result<usize> {
        let Some(stream_id) = self.stream_sending(track_alias, location) else { return Err(Error::SubgroupNotOpen) };
        self.out_streams.get_mut(&stream_id).ok_or(Error::SubgroupNotOpen)?.send_obj_pld(buf, t)
    }

    /// Payload bytes of `location` not yet sent — `None` if no stream is
    /// currently sending it (not started, already done, or torn down).
    pub fn obj_pld_remaining(&self, track_alias: TrackAlias, location: Location) -> Option<usize> {
        let stream_id = self.stream_sending(track_alias, location)?;
        self.out_streams.get(&stream_id)?.remaining_payload()
    }

    /// Resets the stream sending `location`, e.g. its source left the cache
    /// before the payload was sent. Other streams of the Subgroup stay open,
    /// unlike `reset_send_subgroup`.
    pub fn cancel_send_obj(&mut self, track_alias: TrackAlias, location: Location, code: ResetCode, t: &mut impl Transport) {
        let Some(stream_id) = self.stream_sending(track_alias, location) else { return };
        if let Some(track) = self.out_tracks.get_mut(&track_alias) {
            for stream_ids in track.open_streams.values_mut() {
                stream_ids.retain(|&sid| sid != stream_id);
            }
            track.open_streams.retain(|_, ids| !ids.is_empty());
        }
        t.reset_stream(stream_id, code);
        self.out_streams.remove(&stream_id);
    }

    /// Resets every subgroup stream on `track_alias` not yet fully acked.
    /// - for Forward=0 (draft-19 §11.4.3: reset, not FIN)
    /// - includes fully written objects still in the send buffer
    pub fn cancel_unfinished_streams(&mut self, track_alias: TrackAlias, code: ResetCode, t: &mut impl Transport) {
        let Some(track) = self.out_tracks.get_mut(&track_alias) else { return };
        for stream_id in core::mem::take(&mut track.open_streams).into_values().flatten() {
            if !t.stream_finished(stream_id) {
                t.reset_stream(stream_id, code);
            }
            self.out_streams.remove(&stream_id);
        }
    }

    /// Resets (abandons) every open stream for `(group_id, subgroup_id)` on
    /// `track_alias`, if any — e.g. it fell behind
    /// `max_cache_duration`/`delivery_timeout`, or its source object was
    /// evicted from the app's own cache under pressure.
    pub fn reset_send_subgroup(&mut self, track_alias: TrackAlias, group_id: u64, subgroup_id: u64, code: ResetCode, t: &mut impl Transport) {
        let sg = SubgroupLocation { group_id, subgroup_id };
        let Some(stream_ids) = self.out_tracks.get_mut(&track_alias).and_then(|tr| tr.open_streams.remove(&sg)) else { return };
        for stream_id in stream_ids {
            t.reset_stream(stream_id, code);
            self.out_streams.remove(&stream_id);
        }
    }

    /// FINs the open streams of `(group_id, subgroup_id)`. They stay tracked
    /// until delivered (`Transport::stream_finished`) or expired, a FIN only
    /// means no more objects.
    pub fn fin_send_subgroup(&mut self, track_alias: TrackAlias, group_id: u64, subgroup_id: u64, t: &mut impl Transport) {
        let sg = SubgroupLocation { group_id, subgroup_id };
        let Some(stream_ids) = self.out_tracks.get(&track_alias).and_then(|tr| tr.open_streams.get(&sg)) else { return };
        let reset: Vec<u64> = stream_ids.iter().copied()
            .filter(|sid| self.out_streams.get_mut(sid).is_some_and(|s| s.fin(t, self.now)))
            .collect();
        for stream_id in reset {
            self.forget_out_stream(track_alias, sg, stream_id);
        }
        self.note_expiry(track_alias, self.now);
    }

    /// Folds a stream expiry starting at `start` into `earliest_deadline`, so
    /// `timeout()` wakes up for it without waiting for the next sweep.
    fn note_expiry(&mut self, track_alias: TrackAlias, start: Timestamp) {
        let Some(deadline) = self.out_tracks.get(&track_alias).and_then(|track| track.expiry_deadline(start)) else { return };
        self.earliest_deadline = Some(self.earliest_deadline.map_or(deadline, |e| e.min(deadline)));
    }

    /// Every Subgroup currently having an open outgoing stream (opened, not
    /// yet fin'd/reset) on this track.
    pub fn in_progress_send_subgroups(&self, track_alias: TrackAlias) -> Vec<SubgroupLocation> {
        self.out_tracks.get(&track_alias)
            .map(|tr| tr.open_streams.iter()
                .filter(|(_, ids)| ids.iter().any(|sid| self.out_streams.get(sid).is_some_and(|s| !s.fin_sent())))
                .map(|(&sg, _)| sg)
                .collect())
            .unwrap_or_default()
    }

    /// Drops the bookkeeping of a stream that is done, e.g. reset.
    fn forget_out_stream(&mut self, track_alias: TrackAlias, sg: SubgroupLocation, stream_id: u64) {
        if let Some(tr) = self.out_tracks.get_mut(&track_alias)
            && let Some(stream_ids) = tr.open_streams.get_mut(&sg)
        {
            stream_ids.retain(|&sid| sid != stream_id);
            if stream_ids.is_empty() {
                tr.open_streams.remove(&sg);
            }
        }
        self.out_streams.remove(&stream_id);
    }

    /// `subscriber_priority`: SUBSCRIBER_PRIORITY, mapped via `object_urgency`.
    pub fn set_out_track_priority(
        &mut self,
        track_alias: TrackAlias,
        subscriber_priority: u8,
        incremental: bool,
        t: &mut impl Transport,
    ) {
        let urgency = object_urgency(subscriber_priority);
        let Some(track) = self.out_tracks.get_mut(&track_alias) else { return };
        track.urgency = urgency;
        track.incremental = incremental;
        for &stream_id in track.open_streams.values().flatten() {
            t.set_stream_priority(stream_id, urgency, incremental);
        }
    }

    pub fn subscription_delivery_timeout(&self, track_alias: TrackAlias) -> Option<Duration> {
        self.out_tracks.get(&track_alias)?.delivery_timeout
    }

    /// PUBLISH, in Forward State `forward` (draft-ietf-moq-transport-18 §5.1), naming the track's
    /// `largest_location` if it has Objects (§10.10).
    pub fn publish_track(
        &mut self,
        namespace_trackname: NamespaceTrackname,
        track_properties: KeyValuePairs,
        forward: bool,
        largest_location: Option<Location>,
        t: &mut impl Transport,
    ) -> Result<Publication<Q>> {
        let request_id = self.available_request_id()?;
        let track_alias = self.next_out_track_alias;
        let cm = PublishMessage { forward, largest_location, ..PublishMessage::new(request_id, namespace_trackname, track_alias, track_properties) };
        self.send_request(request_id, &ControlMessageEnum::Publish(cm.clone()), t)?;
        self.next_request_id = self.next_request_id.next();
        self.next_out_track_alias.0 += 1;
        let version = self.effective_version();
        self.out_tracks.insert(track_alias, OutTrack::new(DeliveryTimeouts::default(), None, version));
        self.out_requests.insert(request_id, track_alias);
        self.pending_sent_publish.insert(request_id, cm);
        Ok(Publication { obligation: self.obligation(DroppedKind::Publication, request_id), track_alias })
    }

    /// Ends an egress subscription (accepted SUBSCRIBE or sent PUBLISH) with
    /// a PUBLISH_DONE `status`, e.g. `PublishDoneStatus::TrackEnded`.
    /// - FIN'd subgroups keep being delivered, the track is removed once they are
    /// - unfinished subgroups are reset: no object follows, a FIN would claim them complete
    pub fn publish_done(&mut self, mut publication: Publication<Q>, status: PublishDoneStatus, t: &mut impl Transport) {
        let request_id = self.resolve(&mut publication.obligation);
        self.end_publication(request_id, status, t);
    }

    pub(super) fn end_publication(&mut self, request_id: RequestId, status: PublishDoneStatus, t: &mut impl Transport) {
        let Some(track_alias) = self.out_requests.remove(&request_id) else { return };
        let Some(track) = self.out_tracks.get_mut(&track_alias) else { return };
        // removed by the sweep in `handle()` once its streams are done
        track.ended = true;
        let stream_count = track.streams_opened;
        self.reset_open_subgroups(track_alias, t);
        self.send_final_response(
            request_id,
            &ControlMessageEnum::PublishDone(PublishDoneMessage::new(request_id, status, stream_count)),
            t,
        )
    }

    /// Resets the subgroup streams of `track_alias` not FIN'd yet.
    fn reset_open_subgroups(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        let open: Vec<(SubgroupLocation, u64)> = self.out_tracks[&track_alias].open_streams.iter()
            .flat_map(|(&sg, ids)| ids.iter().map(move |&stream_id| (sg, stream_id)))
            .filter(|(_, stream_id)| self.out_streams.get(stream_id).is_none_or(|s| !s.fin_sent()))
            .collect();
        for (sg, stream_id) in open {
            t.reset_stream(stream_id, ResetCode::Canceled);
            self.forget_out_stream(track_alias, sg, stream_id);
        }
    }

    /// Resets the not yet delivered streams of an egress track and forgets it.
    pub(super) fn remove_out_track(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        self.cancel_unfinished_streams(track_alias, ResetCode::Canceled, t);
        self.out_tracks.remove(&track_alias);
    }
}

//! Upstream subscriptions to the cache.

use super::*;

/// Drives every MoQ subscription feeding `track`'s cache forward one tick.
/// - Sends SUBSCRIBE for active subscriptions (passive ones wait for an
///   incoming PUBLISH — see `answer_publish`).
/// - Polls for the alias, reads available objects into the cache — merging
///   via `moq_object_cache::{begin_or_join, fold}` when several subscriptions
///   fill the same object (multi-source/multipath; a no-op with just one).
/// - Drops a subscription that ends (rejected, `PUBLISH_DONE`, malformed
///   track); if that empties `track.subscriptions`, marks every
///   `LocalAliasState` `gone` for the finalize pass to `PUBLISH_DONE`.
pub(super) fn drive_track_subscriptions(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) -> (bool, Option<ClientId>) {
    if track.subscriptions.is_empty() { return (false, None); }
    let mut progress = false;
    // Set the moment any subscription racing for `nt` (the incumbent leader
    // or a `trigger_race` participant) is the first to land a real Object
    // at or after the race's target Location — see the `begin_or_join`
    // success arm below. Resolved by the caller (`resolve_race` needs the
    // whole `&mut LocalTrack` back, which this function's own destructured
    // borrow below can't give up mid-call).
    let mut race_winner: Option<ClientId> = None;
    let LocalTrack { cache, subscriptions, aliases, max_cache_duration, race, stats, .. } = track;
    let mut ended: Vec<usize> = Vec::new();
    // PUBLISH_DONE status for the subscribers if the last subscription ends
    let mut end_status = PublishDoneStatus::TrackEnded;
    // objects a subscription stopped filling mid-payload
    let mut abandoned: Vec<Location> = Vec::new();
    for (i, sub) in subscriptions.iter_mut().enumerate() {
        if sub.request_id.is_none() {
            if sub.passive { continue; } // wait for incoming PUBLISH to set track_alias
            let Some((mut moq, handles)) = conn_for(conns, sub.client_id) else { continue };
            // the relay's own configured timeout, not its subscribers' (draft-16 §9.2.2.2)
            match moq.session.subscribe(nt, sub.options, &mut moq.t) {
                Ok(subscription) => {
                    sub.request_id = Some(subscription.request_id());
                    handles.subscriptions.insert(subscription.request_id(), subscription);
                    info!("conn {} subscribe {nt}", sub.client_id);
                }
                Err(quiche_moq::Error::SessionClosed) => {
                    info!("conn {} subscribe {nt}: session closed", sub.client_id);
                    ended.push(i);
                    continue;
                }
                Err(e) => { error!("subscribe {nt}: {e:?}"); continue; }
            }
        }
        if sub.track_alias.is_none()
            && let Some(rid) = sub.request_id {
                let Some(responses) = responses_for(conns, sub.client_id) else { continue };
                match responses.take_subscribe(rid) {
                    Some(Ok((alias, ok_msg))) => {
                        sub.track_alias = Some(alias);
                        sub.start = Some(FilterRange::resolve(sub.options.filter, ok_msg.largest_location()).start.unwrap_or(Location { group: 0, object: 0 }));
                        if max_cache_duration.is_none() { *max_cache_duration = ok_msg.max_cache_duration(); }
                        info!("conn {} accepted subscribe {nt}", sub.client_id);
                    }
                    Some(Err(e)) => {
                        // Upstream (or #upstream fallback) doesn't have this
                        // namespace after all — this subscription is done for.
                        error!("subscribe rejected {nt}: {e}");
                        ended.push(i);
                        continue;
                    }
                    None => {}
                }
            }
        let Some(alias) = sub.track_alias else { continue };
        let Some(mut moq) = moq_handle_for(conns, sub.client_id) else { continue };
        let before = abandoned.len();
        let mut into = Ingest { nt, cache, aliases, stats, race, race_winner: &mut race_winner, abandoned: &mut abandoned };
        let drained = drain_subscription(moq.session, &mut moq.t, alias, sub, &mut into);
        // a `#race` standby resumes at the leader's partial objects (`replace_lost_leader`)
        if drained.ended.is_some() && sub.race && race.as_ref().is_some_and(RaceState::has_standby) {
            abandoned.truncate(before);
        }
        progress |= drained.progress;
        if let Some(status) = drained.ended {
            end_status = status;
            ended.push(i);
        }
    }
    for &i in ended.iter().rev() {
        let sub = subscriptions.remove(i);
        unsubscribe_upstream(conns, sub.client_id, sub.request_id);
    }
    for loc in abandoned {
        cancel_abandoned(cache, subscriptions, stats, loc);
    }
    // a `#race` standby takes over (`replace_lost_leader`)
    if !ended.is_empty() && subscriptions.is_empty() && !race.as_ref().is_some_and(RaceState::has_standby) {
        for a in aliases.iter_mut() { a.publish_done = Some(end_status); }
    }
    (progress, race_winner)
}

/// What one `drain_subscription` call changed, for the caller to act on.
pub(super) struct Drained {
    /// Something was read into the cache, so forwarding may have new work.
    progress: bool,
    /// The subscription is finished and should be removed, with the
    /// PUBLISH_DONE status to pass downstream.
    ended: Option<PublishDoneStatus>,
}

/// Largest payload read per `recv_obj_pld` call in `drain_subscription`.
pub(super) const RECV_CHUNK: usize = 16 * 1024;

/// Where a subscription's objects go: its track's cache and bookkeeping.
pub(super) struct Ingest<'a> {
    nt: &'a NamespaceTrackname,
    cache: &'a mut PackedObjectCache,
    aliases: &'a mut [LocalAliasState],
    stats: &'a mut TrackStats,
    race: &'a Option<RaceState>,
    /// Set by the first real object at an in-flight `Data` race's target.
    race_winner: &'a mut Option<ClientId>,
    /// Objects a subscription can no longer finish, see `cancel_abandoned`.
    abandoned: &'a mut Vec<Location>,
}

/// What one read pass did.
#[derive(Default)]
pub(super) struct Pass {
    /// An object was opened or read further.
    advanced: bool,
    progress: bool,
}

/// Reads everything currently available on one subscription into its track.
/// - takes `moq_core` types to be testable on `MockPipe`
/// - objects read independently (`in_progress`), subgroups must not block each other
/// - passes until nothing changes: a finished object can expose the next header
pub(super) fn drain_subscription(
    session: &mut moq_core::MoqTransportSession,
    t: &mut impl moq_core::Transport,
    alias: quiche_moq::wire::TrackAlias,
    sub: &mut TrackSubscription,
    into: &mut Ingest,
) -> Drained {
    let mut progress = false;
    let mut scratch = [0u8; RECV_CHUNK];
    let ended = loop {
        let (headers, ended) = read_headers(session, t, alias, sub, into);
        progress |= headers.progress;
        if ended.is_some() { break ended; }
        let payloads = read_payloads(session, t, alias, sub, into, &mut scratch);
        progress |= payloads.progress;
        if !(headers.advanced || payloads.advanced) { break None; }
    };
    let ended = ended.or_else(|| session.recv_track_fully_done(alias).then_some(PublishDoneStatus::TrackEnded));
    if ended.is_some() {
        into.abandoned.extend(sub.in_progress.drain(..).map(|(loc, _)| loc));
    }
    Drained { progress, ended }
}

/// Opens every object whose header is available. The PUBLISH_DONE status if
/// the subscription can't go on.
pub(super) fn read_headers(
    session: &mut moq_core::MoqTransportSession,
    t: &mut impl moq_core::Transport,
    alias: quiche_moq::wire::TrackAlias,
    sub: &mut TrackSubscription,
    into: &mut Ingest,
) -> (Pass, Option<PublishDoneStatus>) {
    let mut pass = Pass::default();
    loop {
        match session.recv_new_obj_hdr(alias, t) {
            Ok(moq_core::RecvObjHdr::Header { location, subgroup_id, publisher_priority, end_of_group, header }) => {
                let object = NewObject { location, subgroup_id, publisher_priority, end_of_subgroup: false, properties: header.properties().clone(), payload_len: header.payload_len() };
                match begin_or_join(into.cache, object) {
                    Ok(evicted) => {
                        // An evicted object still arriving on another
                        // subscription is dropped there: `fold` finds no entry.
                        cache_to_moq::mark_evicted(into.aliases.iter_mut().map(|a| &mut a.state), &evicted);
                        if end_of_group {
                            into.cache.mark_end_of_group(location);
                        }
                        // cached as received (draft-ietf-moq-transport-18 §11.2.1.1)
                        if let Some(status) = header.status()
                            && let Some(mut object) = into.cache.find_mut(location)
                        {
                            object.set_status(status);
                        }
                        into.stats.record(header.payload_len());
                        sub.in_progress.push((location, header.payload_len()));
                        pass.advanced = true;
                        // First real Object at or after an in-flight race's
                        // target from either the incumbent leader (`sub.race`)
                        // or a raced participant wins, unless
                        // `poll_active_race` already decided by response.
                        // `Data` only: `Keep`/`Switch` decide by REQUEST_UPDATE_OK.
                        if into.race_winner.is_none()
                            && let Some(r) = into.race.as_ref()
                            && r.mode == crate::args::RaceMode::Data
                            && let Some(active) = r.active.as_ref()
                            && (sub.race || active.participants.contains_key(&sub.client_id))
                            && location >= active.target
                        {
                            *into.race_winner = Some(sub.client_id);
                        }
                    }
                    // Cache couldn't fit it, or another subscription already
                    // filling this Location disagreed on its content —
                    // nothing to read it into either way, so don't leave the
                    // upstream stream hanging.
                    Err(JoinError::CacheFull | JoinError::Mismatch) => {
                        session.cancel_recv_obj(alias, location, t);
                        into.stats.canceled += 1;
                    }
                }
            }
            // The real "no more coming" signal `cache_to_moq::forward`
            // needs to FIN its own downstream copy — replaces guessing
            // this from cache contents.
            Ok(moq_core::RecvObjHdr::SubgroupFinished { location, .. }) => {
                into.cache.mark_end_of_subgroup(location);
                pass.progress = true;
            }
            Ok(moq_core::RecvObjHdr::SubgroupReset { location, .. }) => {
                into.cache.mark_subgroup_reset(location);
                pass.progress = true;
            }
            Ok(moq_core::RecvObjHdr::Datagram { location, publisher_priority, end_of_group, status, properties, payload }) => {
                match put_datagram(into.cache, location, publisher_priority, end_of_group, status, properties, &payload) {
                    Ok(evicted) => {
                        cache_to_moq::mark_evicted(into.aliases.iter_mut().map(|a| &mut a.state), &evicted);
                        into.stats.record(payload.len());
                        pass.progress = true;
                        pass.advanced = true;
                    }
                    Err(JoinError::CacheFull | JoinError::Mismatch) => into.stats.canceled += 1,
                }
            }
            Err(moq_core::Error::Done) => return (pass, None),
            // no new objects; open ones still read below
            Err(moq_core::Error::Fin) => return (pass, None),
            Err(moq_core::Error::MalformedTrack) => {
                // draft-19 §9.1/§2.4.2: a relay that detects a malformed
                // track must stop relaying it.
                error!("malformed track {} from {}: ending subscription", into.nt, sub.client_id);
                return (pass, Some(PublishDoneStatus::MalformedTrack));
            }
            Err(e) => { error!("recv_new_obj_hdr {}: {e:?}", into.nt); return (pass, None); }
        }
    }
}

/// Reads whatever payload arrived for every open object.
pub(super) fn read_payloads(
    session: &mut moq_core::MoqTransportSession,
    t: &mut impl moq_core::Transport,
    alias: quiche_moq::wire::TrackAlias,
    sub: &mut TrackSubscription,
    into: &mut Ingest,
    scratch: &mut [u8; RECV_CHUNK],
) -> Pass {
    let mut pass = Pass::default();
    let mut j = 0;
    while j < sub.in_progress.len() {
        let (loc, payload_len) = sub.in_progress[j];
        let received = session.obj_pld_received(alias, loc).unwrap_or(0);
        if received >= payload_len {
            // e.g. zero-length object
            sub.in_progress.swap_remove(j);
            continue;
        }
        let chunk = &mut scratch[..(payload_len - received).min(RECV_CHUNK)];
        match session.recv_obj_pld(alias, loc, chunk, t) {
            Ok(n) if n > 0 => {
                fold(into.cache, loc, received + n, &chunk[..n]);
                pass.progress = true;
                pass.advanced = true;
                if received + n >= payload_len {
                    sub.in_progress.swap_remove(j);
                    continue;
                }
            }
            Ok(_) | Err(moq_core::Error::Done) => {}
            Err(_) => {
                // reset mid-object, nothing left to finish it from this source
                sub.in_progress.swap_remove(j);
                into.abandoned.push(loc);
                pass.progress = true;
                pass.advanced = true;
                continue;
            }
        }
        j += 1;
    }
    pass
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    use quiche_moq::wire::object::ObjectStatus;
    use moq_core::Transport;
    use moq_core::test_utils::{init_mock_pipe, subscribed_track};
    use moq_core::wire::{KeyValuePairs, MOQ_VERSION_DRAFT_16};
    use moq_core::{Config, OutgoingObject, VersionNegotiation};
    use moq_object_cache::ObjectState;

    /// A partial object must not keep a complete one on a later stream out of the cache.
    #[test]
    fn complete_object_not_held_back_by_partial_one() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_mock_pipe(config);
        let alias = subscribed_track(&mut p);
        let kvps = KeyValuePairs::new();

        // partial: 4 of 8 bytes, earlier stream
        let partial = Location { group: 0, object: 0 };
        p.server.send_obj_hdr(alias, OutgoingObject { end_of_group: true, properties: Some(&kvps), ..OutgoingObject::new(partial, 0, 8) }, &mut p.server_t).unwrap();
        p.server.send_obj_pld(alias, partial, b"abcd", &mut p.server_t).unwrap();

        // complete, later stream
        let complete = Location { group: 1, object: 0 };
        p.server.send_obj_hdr(alias, OutgoingObject { end_of_group: true, properties: Some(&kvps), ..OutgoingObject::new(complete, 0, 3) }, &mut p.server_t).unwrap();
        p.server.send_obj_pld(alias, complete, b"xyz", &mut p.server_t).unwrap();
        p.advance();

        let mut sub = TrackSubscription::new(0, false, SubscribeOptions::default());
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let mut stats = TrackStats::default();
        let mut race_winner = None;
        let mut abandoned = Vec::new();
        let nt: NamespaceTrackname = "n1--t1".parse().unwrap();
        let drained = drain_subscription(&mut p.client, &mut p.client_t, alias, &mut sub, &mut Ingest {
            nt: &nt, cache: &mut cache, aliases: &mut [], stats: &mut stats, race: &None, race_winner: &mut race_winner, abandoned: &mut abandoned,
        });

        assert!(drained.progress);
        assert_eq!(
            cache.find(complete).map(|o| o.state()), Some(ObjectState::Completed),
            "a complete object on a later stream must reach the cache despite an earlier partial one",
        );
        assert_eq!(cache.find(partial).map(|o| o.state()), Some(ObjectState::Progressing));
        assert_eq!(sub.in_progress, vec![(partial, 8)], "only the partial object should remain open");

        // rest of the partial object
        p.server.send_obj_pld(alias, partial, b"efgh", &mut p.server_t).unwrap();
        p.advance();
        drain_subscription(&mut p.client, &mut p.client_t, alias, &mut sub, &mut Ingest {
            nt: &nt, cache: &mut cache, aliases: &mut [], stats: &mut stats, race: &None, race_winner: &mut race_winner, abandoned: &mut abandoned,
        });
        assert_eq!(cache.find(partial).map(|o| o.state()), Some(ObjectState::Completed));
        assert!(sub.in_progress.is_empty());
        assert!(abandoned.is_empty());
    }

    /// End of Group is cached with its status (draft-ietf-moq-transport-18 §11.2.1.1).
    #[test]
    fn object_status_is_cached() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_mock_pipe(config);
        let alias = subscribed_track(&mut p);
        assert!(alias.0 < 64, "single byte varint");
        let stream_id = p.server_t.open_uni_stream().unwrap();
        // header | object id, extensions length, payload length 0, status
        p.server_t.send_stream(stream_id, &[0x15, alias.0 as u8, 0, 0, 0, 0, 0, 0, 0x3], false).unwrap();
        p.advance();

        let mut sub = TrackSubscription::new(0, false, SubscribeOptions::default());
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let nt: NamespaceTrackname = "n1--t1".parse().unwrap();
        drain_subscription(&mut p.client, &mut p.client_t, alias, &mut sub, &mut Ingest {
            nt: &nt, cache: &mut cache, aliases: &mut [], stats: &mut TrackStats::default(), race: &None, race_winner: &mut None, abandoned: &mut Vec::new(),
        });

        let object = cache.find(Location { group: 0, object: 0 });
        assert_eq!(object.map(|o| o.status()), Some(ObjectStatus::EndOfGroup));
    }

    /// A datagram counts like an object on a stream, rate included.
    #[test]
    fn datagram_is_recorded() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_mock_pipe(config);
        let alias = subscribed_track(&mut p);
        let kvps = KeyValuePairs::new();
        let object = moq_core::OutgoingDatagram { location: Location { group: 0, object: 0 }, publisher_priority: 0, end_of_group: false, status: ObjectStatus::Normal, properties: &kvps, received_at: None, payload: b"abc" };
        p.server.send_datagram_obj(alias, object, &mut p.server_t).unwrap();
        p.advance();

        let mut sub = TrackSubscription::new(0, false, SubscribeOptions::default());
        let mut stats = TrackStats::default();
        let nt: NamespaceTrackname = "n1--t1".parse().unwrap();
        drain_subscription(&mut p.client, &mut p.client_t, alias, &mut sub, &mut Ingest {
            nt: &nt, cache: &mut PackedObjectCache::new(4, 64, 64), aliases: &mut [], stats: &mut stats, race: &None, race_winner: &mut None, abandoned: &mut Vec::new(),
        });

        assert_eq!((stats.objects, stats.bytes), (1, 3));
        assert!(stats.first_at.is_some() && stats.last_at.is_some());
    }

    /// END_OF_GROUP is the Subgroup header's, not whether an Object ends its
    /// Subgroup (draft-ietf-moq-transport-18 §11.4.2).
    #[rstest::rstest]
    fn end_of_group_is_cached_as_received(#[values(false, true)] end_of_group: bool) {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_mock_pipe(config);
        let alias = subscribed_track(&mut p);
        let kvps = KeyValuePairs::new();
        let location = Location { group: 0, object: 0 };
        p.server.send_obj_hdr(alias, OutgoingObject { end_of_group, properties: Some(&kvps), ..OutgoingObject::new(location, 0, 1) }, &mut p.server_t).unwrap();
        p.server.send_obj_pld(alias, location, b"a", &mut p.server_t).unwrap();
        p.server.fin_send_subgroup(alias, 0, 0, &mut p.server_t);
        p.advance();

        let mut sub = TrackSubscription::new(0, false, SubscribeOptions::default());
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let nt: NamespaceTrackname = "n1--t1".parse().unwrap();
        drain_subscription(&mut p.client, &mut p.client_t, alias, &mut sub, &mut Ingest {
            nt: &nt, cache: &mut cache, aliases: &mut [], stats: &mut TrackStats::default(), race: &None, race_winner: &mut None, abandoned: &mut Vec::new(),
        });

        let object = cache.find(location).unwrap();
        assert!(object.end_of_subgroup());
        assert_eq!(object.end_of_group(), end_of_group);
    }
}

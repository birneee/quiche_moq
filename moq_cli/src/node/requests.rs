//! Answering the peers' requests, and the PUBLISH_NAMESPACEs and PUBLISHes this node sends.

use super::*;
use crate::args::SubArg;

/// Whether a downstream SUBSCRIBE for `local` can be answered now: it has no
/// upstream subscription, or one resolved. See `PendingSubscribe`.
pub(super) fn local_track_ready(local: &LocalTrack) -> bool {
    local.subscriptions.is_empty() || local.subscriptions.iter().any(|s| s.track_alias.is_some())
}

/// Accepts a SUBSCRIBE for a locally-served or relayed track.
/// - Creates `LocalTrack` if needed.
/// - Ensures a `TrackSubscription` toward `relay_via`, no-op if one for that
///   `client_id` exists.
/// - If the track isn't `local_track_ready` yet (a fresh relayed track whose
///   upstream subscription hasn't resolved), defers to `local.pending_subscribes`
///   instead of answering immediately — `drive_pending_subscribes` resolves it
///   later. Otherwise registers the new alias for cache forwarding right away.
pub(super) fn accept_local_subscription(
    moq: &mut MoqHandle, handles: &mut Handles, ad: &mut AppData, cid: ClientId, request: Request<Subscribe>, relay_via: Option<ClientId>,
) {
    let nt = &request.message().namespace_trackname.clone();
    if !ad.local_tracks.contains_key(nt) {
        let mut t = ad.new_local_track();
        t.relayed = relay_via.is_some();
        ad.local_tracks.insert(nt.clone(), t);
    }
    let local = ad.local_tracks.get_mut(nt).unwrap();
    if let Some(pub_id) = relay_via
        && !local.subscriptions.iter().any(|s| s.client_id == pub_id)
    {
        // draft-16 §9.2.2.2: a relay SHOULD NOT forward a subscriber's delivery timeout upstream
        let forward = local.upstream_forward(ad.prewarm, [request.message().forward]) == Some(true);
        // an absolute start goes upstream as is, a later subscriber starting before gets the rest by `fill`
        let filter = match request.message().filter.start_location() {
            Some(start) => LocationFilter::AbsoluteStart { start },
            None => SubArg::default_options().filter,
        };
        local.subscriptions.push(TrackSubscription::relayed(pub_id, SubscribeOptions { forward, filter, ..SubArg::default_options() }));
    }
    if !local_track_ready(local) {
        local.pending_subscribes.push(PendingSubscribe { client_id: cid, request, largest: local.largest() });
        info!("conn {cid} deferring subscription {nt} (waiting for upstream)");
        return;
    }
    let largest = local.largest();
    finish_accept_local_subscription(moq, handles, local, cid, request, largest);
}

/// The actual accept: registers the alias and sends SUBSCRIBE_OK. Split out
/// of `accept_local_subscription` so `drive_pending_subscribes` can call it
/// once a deferred track becomes ready, without redoing the `relay_via`/
/// `LocalTrack`-creation steps (already done when the SUBSCRIBE first
/// arrived). `largest` is the Largest Object reported in SUBSCRIBE_OK, the
/// filter resolves against it (draft-16 §5.1.2).
pub(super) fn finish_accept_local_subscription(
    moq: &mut MoqHandle, handles: &mut Handles, local: &mut LocalTrack, cid: ClientId, request: Request<Subscribe>, largest: Option<Location>,
) {
    let sub = request.message();
    let (nt, filter, priority, forward) = (sub.namespace_trackname.clone(), sub.filter, sub.subscriber_priority, sub.forward);
    let request_id = request.request_id();
    let ok = SubscribeOk { largest_location: largest, max_cache_duration: local.max_cache_duration };
    let Ok(publication) = moq.session.accept_request(request, ok, &mut moq.t) else {
        info!("conn {cid} subscription {nt} cancelled before it was answered");
        return;
    };
    let alias = publication.track_alias();
    handles.publications.insert(request_id, publication);
    moq.session.set_out_track_priority(alias, priority, false, &mut moq.t);
    let range = FilterRange::resolve(filter, largest);
    // what the upstream subscription doesn't deliver is fetched (`fill`)
    // TODO also for a REQUEST_UPDATE to Forward 1 or to an earlier start, it changes the testbed's race
    //  switches, implement when needed
    if let Some(start) = range.start.filter(|_| forward && local.relayed) {
        local.fill.want(start);
    }
    local.aliases.push(LocalAliasState { conn_id: cid, alias, request_id, priority, publish_done: None, forward, joining_location: largest, state: CacheToMoq::filtered(range) });
    info!("conn {cid} accept subscription {nt}");
}

/// Drops the subscriber on `cid` that sent UNSUBSCRIBE, moq_core already
/// reset its streams.
pub(super) fn apply_unsubscribe(cid: ClientId, alias: TrackAlias, ad: &mut AppData) {
    for (nt, track) in ad.local_tracks.iter_mut() {
        let before = track.aliases.len();
        track.aliases.retain(|a| !(a.conn_id == cid && a.alias == alias));
        if track.aliases.len() < before {
            info!("conn {cid} unsubscribed {nt}");
            break;
        }
    }
}

/// Drops a deferred SUBSCRIBE the subscriber on `cid` withdrew.
pub(super) fn drop_withdrawn_subscribe(cid: ClientId, request_id: RequestId, ad: &mut AppData) {
    ad.rendezvous.retain(|held| !(held.client_id == cid && held.request.request_id() == request_id));
    for track in ad.local_tracks.values_mut() {
        track.pending_subscribes.retain(|p| !(p.client_id == cid && p.request.request_id() == request_id));
    }
}

/// Applies a downstream subscriber's REQUEST_UPDATE changing Forward State
/// and/or SUBSCRIPTION_FILTER (e.g. a `#race` promotion) to its `LocalAliasState`.
/// - filter change moves `CacheToMoq`'s `range`, see `CacheToMoq::refilter`
pub(super) fn apply_subscription_update(cid: ClientId, request_id: RequestId, forward: Option<bool>, filter: Option<LocationFilter>, ad: &mut AppData) {
    for track in ad.local_tracks.values_mut() {
        let largest = track.largest();
        let Some(als) = track.aliases.iter_mut().find(|a| a.conn_id == cid && a.request_id == request_id)
        else { continue };
        als.update(largest, forward, filter);
        break;
    }
}

/// The subscriber's PUBLISH_OK to our PUBLISH `request_id` of `track`: its Forward State, filter and
/// priority (draft-ietf-moq-transport-18 §10.5), a relative filter against the Largest Location the PUBLISH named.
// TODO its delivery timeouts
fn apply_publish_ok(cid: ClientId, moq: &mut MoqHandle, request_id: RequestId, ok: &PublishOkMessage, track: &mut LocalTrack) {
    let Some(als) = track.aliases.iter_mut().find(|a| a.conn_id == cid && a.request_id == request_id) else { return };
    let published_largest = als.joining_location;
    als.update(published_largest, Some(ok.forward), Some(ok.filter));
    als.priority = ok.subscriber_priority;
    moq.session.set_out_track_priority(als.alias, als.priority, false, &mut moq.t);
}

/// Answers a REQUEST_UPDATE with the LARGEST_OBJECT of the track it updates,
/// which needs the cache.
pub(super) fn answer_request_update(cid: ClientId, moq: &mut MoqHandle, ad: &AppData, request: Request<RequestUpdate>) {
    let existing_request_id = request.message().existing_request_id;
    let largest = ad.local_tracks.values()
        .find(|track| track.aliases.iter().any(|a| a.conn_id == cid && a.request_id == existing_request_id))
        .and_then(|track| track.largest());
    moq.session.accept_request(request, largest, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
}

/// Requests answered in the tick they are handed out can't be cancelled yet.
pub(super) const ANSWERED_ON_ARRIVAL: &str = "a request answered in the tick it arrived is not cancelled";

/// Answers `local.pending_subscribes`: accepted once `local_track_ready`,
/// rejected once every upstream subscription failed. Runs after
/// `drive_track_subscriptions` in the tick.
pub(super) fn drive_pending_subscribes(nt: &NamespaceTrackname, local: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    if local.pending_subscribes.is_empty() { return; }
    // only relayed tracks defer a SUBSCRIBE, so no subscription left means
    // every upstream rejected or disconnected
    let no_upstream = local.subscriptions.is_empty();
    if !no_upstream && !local_track_ready(local) { return; }
    let pending = std::mem::take(&mut local.pending_subscribes);
    for p in pending {
        let Some((mut moq, handles)) = conn_for(conns, p.client_id) else { continue };
        if !no_upstream {
            finish_accept_local_subscription(&mut moq, handles, local, p.client_id, p.request, p.largest);
        } else if moq.session.reject_request(p.request, RequestErrorCode::DoesNotExist, &mut moq.t).is_ok() {
            info!("conn {} reject subscription {nt} (no upstream available)", p.client_id);
        }
    }
}

/// PUBLISH_NAMESPACE on every connection: own `--pub <NS>` namespaces, and
/// with `--relay-namespace` those learned elsewhere, except to their source.
/// - withdrawn (PUBLISH_NAMESPACE_DONE) once gone
/// - `--pub <NS--TRACK>` serves a track without announcing its namespace
pub(super) fn announce_namespaces(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        if !ad.relay_namespace.is_empty() {
            let gone: Vec<Namespace> = conn.app_data.announced_namespaces.keys()
                .filter(|ns| !ad.local_namespaces.contains(ns)
                    && !ad.namespaces.get(ns).is_some_and(|owners| owners.iter().any(|&o| o != cid)))
                .cloned()
                .collect();
            for ns in gone {
                let announcement = conn.app_data.announced_namespaces.remove(&ns).unwrap();
                moq.session.publish_namespace_done(announcement, &mut moq.t);
            }
        }
        let relay_ns = &ad.relay_namespace;
        let relayed = ad.namespaces.keys().filter(|ns| relay_ns.iter().any(|p| p.matches_namespace(ns)));
        for ns in ad.local_namespaces.iter().chain(relayed) {
            if ad.namespaces.get(ns).is_some_and(|owners| owners.contains(&cid)) { continue; }
            if conn.app_data.announced_namespaces.contains_key(ns) { continue; }
            if let Ok(announcement) = moq.session.publish_namespace(ns.clone(), &mut moq.t) {
                conn.app_data.announced_namespaces.insert(ns.clone(), announcement);
                info!("conn {cid} announce namespace {ns}");
            }
        }
    }
}

/// Relayed upstream subscriptions deliver while a subscriber of the track has Forward State 1,
/// with `--prewarm` always (draft-ietf-moq-transport-19 §9.2).
/// One REQUEST_UPDATE at a time per subscription, the next once it is answered.
pub(super) fn sync_relayed_forward(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    let prewarm = ad.prewarm;
    for (nt, track) in ad.local_tracks.iter_mut() {
        // without subscribers: a PUBLISH keeps delivering into the cache, a relayed SUBSCRIBE ends
        let Some(forward) = track.upstream_forward(prewarm, []) else { continue };
        for sub in track.subscriptions.iter_mut().filter(|s| s.relayed) {
            if let Some(update) = sub.forward_update {
                let Some(responses) = responses_for(conns, sub.client_id) else { continue };
                match responses.take_request_update(update) {
                    None => continue,
                    Some(Ok(_)) => sub.forward_update = None,
                    // the publisher keeps its Forward State: not retried
                    Some(Err(e)) => {
                        error!("conn {} forward update for {nt} failed, staying as is: {e}", sub.client_id);
                        sub.forward_update = None;
                    }
                }
            }
            if sub.options.forward == forward { continue; }
            // not subscribed yet: the SUBSCRIBE carries it; awaiting SUBSCRIBE_OK: once established
            let Some(request_id) = sub.request_id else { sub.options.forward = forward; continue };
            if sub.track_alias.is_none() { continue; }
            let Some((mut moq, handles)) = conn_for(conns, sub.client_id) else { continue };
            let Some(subscription) = handles.subscriptions.get(&request_id) else { continue };
            if let Ok(update) = moq.session.request_update_forward(subscription, forward, None, &mut moq.t) {
                info!("conn {} forward {forward} upstream for {nt}", sub.client_id);
                sub.options.forward = forward;
                sub.forward_update = Some(update);
            }
        }
    }
}

/// Answers a SUBSCRIBE_NAMESPACE, `sync_namespace_subscribers` names the namespaces (draft-ietf-moq-transport-18 §10.18).
pub(super) fn answer_subscribe_namespace(cid: ClientId, moq: &mut MoqHandle, handles: &mut Handles, request: Request<SubscribeNamespace>) {
    let prefix = request.message().prefix.clone();
    if handles.namespace_subscriptions.values().any(|s| s.prefix.overlaps(&prefix)) {
        info!("conn {cid} reject namespace subscription {prefix}: overlaps another");
        moq.session.reject_request(request, RequestErrorCode::PrefixOverlap, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        return;
    }
    let request_id = request.request_id();
    let handle = moq.session.accept_request(request, (), &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
    info!("conn {cid} accept namespace subscription {prefix}");
    handles.namespace_subscriptions.insert(request_id, NamespaceSubscriber { prefix, handle, named: HashSet::new() });
}

/// NAMESPACE and NAMESPACE_DONE to each namespace subscriber as the namespaces
/// under its prefix come and go: this node's and the ones announced to it, except by the subscriber.
pub(super) fn sync_namespace_subscribers(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        if conn.app_data.handles.namespace_subscriptions.is_empty() { continue; }
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        let published: HashSet<&Namespace> = ad.local_namespaces.iter()
            .chain(ad.namespaces.iter().filter(|(_, owners)| owners.iter().any(|&o| o != cid)).map(|(ns, _)| ns))
            .collect();
        for subscriber in conn.app_data.handles.namespace_subscriptions.values_mut() {
            let NamespaceSubscriber { prefix, handle, named } = subscriber;
            let new: Vec<&Namespace> = published.iter().copied().filter(|ns| !named.contains(*ns)).collect();
            for ns in new {
                let Some(suffix) = ns.suffix(prefix) else { continue };
                if moq.session.send_namespace(handle, suffix, &mut moq.t).is_ok() {
                    info!("conn {cid} name namespace {ns}");
                    named.insert(ns.clone());
                }
            }
            let gone: Vec<Namespace> = named.iter().filter(|ns| !published.contains(ns)).cloned().collect();
            for ns in gone {
                named.remove(&ns);
                let suffix = ns.suffix(prefix).expect("named under the prefix");
                if moq.session.send_namespace_done(handle, suffix, &mut moq.t).is_ok() {
                    info!("conn {cid} namespace {ns} done");
                }
            }
        }
    }
}

/// Accepts a SUBSCRIBE_TRACKS unless its prefix overlaps another's (draft-ietf-moq-transport-18 §10.19).
pub(super) fn answer_subscribe_tracks(cid: ClientId, moq: &mut MoqHandle, handles: &mut Handles, request: Request<SubscribeTracks>) {
    let (prefix, forward) = (request.message().prefix.clone(), request.message().forward);
    if handles.tracks_subscriptions.values().any(|s| s.prefix.overlaps(&prefix)) {
        info!("conn {cid} reject tracks subscription {prefix}: overlaps another");
        moq.session.reject_request(request, RequestErrorCode::PrefixOverlap, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        return;
    }
    let request_id = request.request_id();
    let handle = moq.session.accept_request(request, (), &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
    info!("conn {cid} accept tracks subscription {prefix}");
    handles.tracks_subscriptions.insert(request_id, TracksSubscriber { prefix, forward, handle, published: HashMap::new() });
}

/// PUBLISH to each tracks subscriber of the tracks under its prefix as they appear, except those
/// it publishes or subscribes to itself (draft-ietf-moq-transport-19 §9.5).
/// - once per track: it ends with PUBLISH_DONE like a subscription when its source ends
pub(super) fn sync_tracks_subscribers(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        if conn.app_data.handles.tracks_subscriptions.is_empty() { continue; }
        let ConnAppData { moq_helper, handles, responses, .. } = &mut conn.app_data;
        let Some(mut moq) = moq_helper.moq_handle(&mut conn.conn) else { continue };
        let Handles { tracks_subscriptions, publications, .. } = handles;
        for subscriber in tracks_subscriptions.values_mut() {
            for (nt, awaiting) in subscriber.published.iter_mut() {
                let Some(request_id) = *awaiting else { continue };
                let Some(answer) = responses.take_publish(request_id) else { continue };
                *awaiting = None;
                let Some(track) = ad.local_tracks.get_mut(nt) else { continue };
                match answer {
                    Ok(ok) => apply_publish_ok(cid, &mut moq, request_id, &ok, track),
                    Err(e) => {
                        info!("conn {cid} rejected publish {nt}: {e}");
                        publications.remove(&request_id);
                        track.aliases.retain(|a| !(a.conn_id == cid && a.request_id == request_id));
                    }
                }
            }
            for (nt, track) in ad.local_tracks.iter_mut() {
                if !nt.namespace().starts_with(&subscriber.prefix) || subscriber.published.contains_key(nt) { continue; }
                let own = track.subscriptions.iter().any(|s| s.client_id == cid)
                    || track.aliases.iter().any(|a| a.conn_id == cid)
                    || track.pending_subscribes.iter().any(|p| p.client_id == cid)
                    || ad.rendezvous.iter().any(|held| held.client_id == cid && &held.request.message().namespace_trackname == nt);
                if own { continue; }
                let largest = track.largest();
                match moq.session.publish_track(nt.clone(), quiche_moq::wire::KeyValuePairs::new(), subscriber.forward, largest, &mut moq.t) {
                    Ok(publication) => {
                        let (request_id, alias) = (publication.request_id(), publication.track_alias());
                        publications.insert(request_id, publication);
                        subscriber.published.insert(nt.clone(), Some(request_id));
                        track.aliases.push(LocalAliasState { conn_id: cid, alias, request_id, priority: DEFAULT_SUBSCRIBER_PRIORITY, publish_done: None, forward: subscriber.forward, joining_location: largest, state: CacheToMoq::new() });
                        info!("conn {cid} publish track {nt} for its tracks subscription");
                    }
                    // no bidirectional stream for it (draft-ietf-moq-transport-18 §6.1)
                    Err(quiche_moq::Error::InsufficientCapacity) => {
                        let suffix = nt.namespace().suffix(&subscriber.prefix).expect("under the prefix");
                        if moq.session.send_publish_blocked(&subscriber.handle, suffix, nt.trackname().to_vec(), &mut moq.t).is_ok() {
                            info!("conn {cid} publish {nt} blocked");
                            subscriber.published.insert(nt.clone(), None);
                        }
                    }
                    // e.g. out of Request IDs for now, retried next tick
                    Err(e) => info!("conn {cid} publish {nt} not sent: {e:?}"),
                }
            }
        }
    }
}

/// PUBLISH of each `--pub` track on every initialized connection.
pub(super) fn send_active_publishes(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    if !ad.active_pub_tracks.is_empty() {
        let (cache_depth, cache_max_obj, cache_avg_obj) = (ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj);
        let all_cids: Vec<ClientId> = conns.iter()
            .map(|(cid, _)| cid)
            .collect();
        for cid in all_cids {
            let entry = ad.client_active_pub_tracks.entry(cid).or_default();
            for nt in &ad.active_pub_tracks {
                if entry.iter().any(|e| &e.track == nt) { continue; }
                entry.push(ClientActivePubTrack { track: nt.clone(), request_id: None });
            }
            let Some(conn) = conns.get_mut(cid) else { continue };
            let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
            let entry = ad.client_active_pub_tracks.get_mut(&cid).unwrap();
            for apt in entry.iter_mut() {
                if apt.request_id.is_none() {
                    let local = ad.local_tracks.entry(apt.track.clone())
                        .or_insert_with(|| LocalTrack::new(cache_depth, cache_max_obj, cache_avg_obj, None));
                    let largest = local.largest();
                    match moq.session.publish_track(apt.track.clone(), quiche_moq::wire::KeyValuePairs::new(), true, largest, &mut moq.t) {
                        Ok(publication) => {
                            let (rid, alias) = (publication.request_id(), publication.track_alias());
                            conn.app_data.handles.publications.insert(rid, publication);
                            apt.request_id = Some(rid);
                            // Unsolicited push (--pub), not a response to a peer's
                            // SUBSCRIBE — `publish_done`
                            // never applies (this track's `subscriptions` is empty
                            // unless it's also independently relay-fed).
                            local.aliases.push(LocalAliasState { conn_id: cid, alias, request_id: rid, priority: DEFAULT_SUBSCRIBER_PRIORITY, publish_done: None, forward: true, joining_location: largest, state: CacheToMoq::new() });
                            info!("conn {cid} publish track {}", apt.track);
                        }
                        // e.g. before SERVER_SETUP: retried next tick
                        Err(moq_core::Error::RequestBlocked) => {}
                        Err(e) => { error!("publish_track {}: {e:?}", apt.track); }
                    }
                } else {
                    let rid = apt.request_id.unwrap();
                    match conn.app_data.responses.take_publish(rid) {
                        Some(Ok(ok)) => {
                            info!("conn {cid} accepted publish {}", apt.track);
                            if let Some(local) = ad.local_tracks.get_mut(&apt.track) {
                                apply_publish_ok(cid, &mut moq, rid, &ok, local);
                            }
                        }
                        Some(Err(e)) => error!("conn {cid} rejected publish {}: {e}", apt.track),
                        None => {}
                    }
                }
            }
        }
    }
}

/// Accepts a PUBLISH if a `--sub`-seeded `TrackSubscription` for `cid` is
/// waiting for it (sets `track_alias` for `drive_track_subscriptions`), or
/// it matches a passive `--pub` wildcard (registers a passive subscription
/// dynamically) — otherwise rejects as uninteresting.
pub(super) fn answer_publish(cid: ClientId, moq: &mut MoqHandle, handles: &mut Handles, ad: &mut AppData, request: Request<Publish>) {
    let (nt, alias, request_id) = (request.message().namespace_trackname.clone(), request.message().track_alias, request.request_id());
    if let Some(local) = ad.local_tracks.get_mut(&nt)
        && let Some(s) = local.subscriptions.iter_mut().find(|s| s.client_id == cid && s.track_alias.is_none())
    {
        s.request_id = Some(request_id);
        s.track_alias = Some(alias);
        info!("conn {cid} accept publish {nt}");
        let subscription = moq.session.accept_request(request, quiche_moq::PublishOk::default(), &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        handles.subscriptions.insert(request_id, subscription);
        return;
    }
    if ad.pub_passive_wildcards.iter().any(|p| p.matches(&nt)) {
        // an ended publisher's objects, nobody's using: the new publisher's may differ at the same locations
        let fed_locally = ad.file_inputs.iter().any(|input| input.track == nt);
        if !fed_locally && ad.local_tracks.get(&nt).is_some_and(LocalTrack::unused) {
            info!("conn {cid} publish {nt} starts the ended track afresh");
            ad.local_tracks.remove(&nt);
        }
        let (cache_depth, cache_max_obj, cache_avg_obj) = (ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj);
        let local = ad.local_tracks.entry(nt.clone())
            .or_insert_with(|| LocalTrack::new(cache_depth, cache_max_obj, cache_avg_obj, None));
        // draft-ietf-moq-transport-19 §9.2: Forward 1 if a subscriber has it; without subscribers too,
        // cached for later ones, Forward 0 only while all are paused (a relay MAY pause until there are some, §9.5)
        // the SUBSCRIBEs held for this publisher are its subscribers too
        let held = ad.rendezvous.iter().filter(|h| h.request.message().namespace_trackname == nt).map(|h| h.request.message().forward);
        let forward = local.upstream_forward(ad.prewarm, held).unwrap_or(true);
        local.subscriptions.push(TrackSubscription::from_accepted_publish(cid, request_id, alias, forward));
        info!("conn {cid} accept publish {nt} (passive --pub wildcard)");
        let ok = quiche_moq::PublishOk { forward, ..Default::default() };
        let subscription = moq.session.accept_request(request, ok, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        handles.subscriptions.insert(request_id, subscription);
        return;
    }
    info!("conn {cid} reject publish {nt}");
    moq.session.reject_request(request, RequestErrorCode::Uninterested, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
}

/// The longest a SUBSCRIBE waits for a publisher, whatever it asks: a relay MAY shorten it (draft-ietf-moq-transport-18 §10.2.6).
const MAX_RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(30);

/// A SUBSCRIBE waiting for a publisher until `deadline`.
pub(super) struct Rendezvous {
    client_id: ClientId,
    request: Request<Subscribe>,
    deadline: Instant,
}

impl AppData {
    /// Whether `cid`'s SUBSCRIBE `request_id` waits for a publisher.
    pub(super) fn rendezvous_holds(&self, cid: ClientId, request_id: RequestId) -> bool {
        self.rendezvous.iter().any(|held| held.client_id == cid && held.request.request_id() == request_id)
    }
}

/// Routes a SUBSCRIBE, else holds it for its RENDEZVOUS_TIMEOUT or rejects it (draft-ietf-moq-transport-18 §10.2.6).
pub(super) fn answer_subscribe(cid: ClientId, moq: &mut MoqHandle, handles: &mut Handles, ad: &mut AppData, request: Request<Subscribe>) {
    let Some(request) = route_subscribe(cid, moq, handles, ad, request) else { return };
    let nt = request.message().namespace_trackname.clone();
    match request.message().rendezvous_timeout.filter(|&ms| ms > 0) {
        Some(ms) => {
            let wait = Duration::from_millis(ms).min(MAX_RENDEZVOUS_TIMEOUT);
            info!("conn {cid} holding subscription {nt} up to {wait:?} for a publisher");
            ad.rendezvous.push(Rendezvous { client_id: cid, deadline: Instant::now() + wait, request });
        }
        None => {
            info!("conn {cid} reject subscription {nt}");
            moq.session.reject_request(request, RequestErrorCode::DoesNotExist, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        }
    }
}

/// Routes the held SUBSCRIBEs whose publisher appeared, rejects the expired, the time until the next expires.
pub(super) fn drive_rendezvous(r: &mut Runner, now: Instant) -> Option<Duration> {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    let mut next = None;
    for held in std::mem::take(&mut ad.rendezvous) {
        let Some((mut moq, handles)) = conn_for(conns, held.client_id) else { continue };
        let Some(request) = route_subscribe(held.client_id, &mut moq, handles, ad, held.request) else { continue };
        if held.deadline <= now {
            info!("conn {} reject subscription {}: no publisher in time", held.client_id, request.message().namespace_trackname);
            moq.session.reject_request(request, RequestErrorCode::Timeout, &mut moq.t).ok();
        } else {
            next = earlier(next, held.deadline - now);
            ad.rendezvous.push(Rendezvous { request, ..held });
        }
    }
    next
}

/// Accepts a SUBSCRIBE that has a source, else hands it back.
fn route_subscribe(
    cid: ClientId, moq: &mut MoqHandle, handles: &mut Handles, ad: &mut AppData, request: Request<Subscribe>,
) -> Option<Request<Subscribe>> {
    let nt = request.message().namespace_trackname.clone();
    // Explicitly registered track (passive --pub / active --pub) — serve locally.
    // (passive --pub's namespace/global wildcard form is a *different*
    // policy — accepting an incoming PUBLISH push dynamically, see
    // `answer_publish` — and must not also short-circuit SUBSCRIBE handling
    // here, or a pure relay with `--pub "*"#passive` would swallow every
    // SUBSCRIBE into an empty, unfed local track instead of relay-forwarding it.)
    if ad.local_tracks.contains_key(&nt) {
        accept_local_subscription(moq, handles, ad, cid, request, None);
        return None;
    }
    // Known namespace (via another peer's announcement or an #upstream
    // connection), matched by a passive --sub namespace/global wildcard:
    // accept immediately and relay-forward it — `accept_local_subscription`
    // ensures a `TrackSubscription` toward `pub_id` exists.
    if ad.sub_passive_wildcards.iter().any(|p| p.matches(&nt))
        && let Some(pub_id) = pick_one(ad, nt.namespace())
    {
        accept_local_subscription(moq, handles, ad, cid, request, Some(pub_id));
        return None;
    }
    Some(request)
}

// TODO: namespace announcements are currently gated only by whether any
// passive --sub wildcard is configured (all-or-nothing). Add a mechanism to
// restrict *who* may announce / *which* namespaces are accepted (e.g. an
// allow-list or per-connection policy), instead of trusting any connected
// peer once relay-forwarding is on.
/// Accepts every namespace an `#upstream` connection announces, otherwise
/// the ones this node relays or subscribes to.
pub(super) fn answer_publish_namespace(
    cid: ClientId, moq: &mut MoqHandle, ad: &mut AppData, request: Request<PublishNamespace>, upstream: bool,
) {
    let ns = request.message().track_namespace().clone();
    let want = upstream
        || !ad.sub_passive_wildcards.is_empty()
        || ad.relay_namespace.iter().any(|p| p.matches_namespace(&ns))
        || ad.sub_track_templates.iter().any(|t| t.track.namespace().has_prefix(&ns));
    if !want {
        info!("conn {cid} reject namespace {ns}");
        moq.session.reject_request(request, RequestErrorCode::Uninterested, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
        return;
    }
    ad.namespaces.entry(ns.clone()).or_default().insert(cid);
    if upstream {
        info!("conn {cid} upstream announced namespace {ns}");
    } else {
        info!("conn {cid} accept namespace {ns}");
    }
    sync_sub_track_templates(ad);
    moq.session.accept_request(request, (), &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
}

/// `cid` withdrew its announcement of `ns`, new subscriptions no longer route to it.
pub(super) fn forget_announcer(cid: ClientId, ns: &Namespace, ad: &mut AppData) {
    info!("conn {cid} withdrew namespace {ns}");
    if let Some(owners) = ad.namespaces.get_mut(ns) {
        owners.remove(&cid);
        if owners.is_empty() { ad.namespaces.remove(ns); }
    }
}

/// SUBSCRIBE and PUBLISH on an `#upstream` connection: this node only receives from upstream.
pub(super) fn reject_from_upstream<K: RequestKind>(cid: ClientId, moq: &mut MoqHandle, request: Request<K>) {
    info!("conn {cid} reject request {} from upstream", request.request_id());
    moq.session.reject_request(request, RequestErrorCode::NotSupported, &mut moq.t).expect(ANSWERED_ON_ARRIVAL);
}

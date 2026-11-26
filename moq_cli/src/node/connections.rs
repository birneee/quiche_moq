//! Connection lifecycle: shutdown, timers, signals, migration, cleanup and what each connection received.

use super::*;

/// `--reconnect`: delay before re-dialing an upstream that could not be connected.
pub(super) const RECONNECT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// SIGINT/SIGTERM: closes every session (CLOSE_WEBTRANSPORT_SESSION,
/// draft-16 §3.4) and connection, then stops the runner.
pub(super) fn shutdown(r: &mut Runner) {
    for (nt, track) in r.endpoint.app_data().local_tracks.iter() {
        info!("{}", format_track_stats(nt, &track.stats));
    }
    let (conns, _) = r.endpoint.mut_conns_and_app_data();
    for (_, conn) in conns.iter_mut() {
        if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
            moq.t.close_session(SessionCloseCode::NoError, "");
        }
        let _ = conn.conn.close(false, 0x0, b"");
    }
    r.close();
}

/// When keepalives and the sessions' timers need the loop to wake without I/O.
pub(super) fn connection_timeouts(r: &mut Runner) -> Option<Duration> {
    let mut app_timeout: Option<Duration> = None;
    let now = Instant::now();
    let (conns, _ad) = r.endpoint.mut_conns_and_app_data();
    for (_, conn) in conns.iter_mut() {
        if let Some(keepalive) = &mut conn.app_data.keepalive
            && let Some(remaining) = keepalive.handle(&mut conn.conn, now) {
            app_timeout = earlier(app_timeout, remaining);
        }
        if let Some(moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn)
            && let Some(remaining) = moq.timeout() {
            app_timeout = earlier(app_timeout, remaining);
        }
    }
    app_timeout
}

/// The `SigAction`s of the signals since the last tick, each once.
pub(super) fn take_signal_actions(r: &mut Runner) -> HashSet<SigAction> {
    let ad = r.endpoint.app_data_mut();
    let mut actions = HashSet::new();
    if ad.usr1_requested {
        ad.usr1_requested = false;
        actions.extend(ad.sigusr1_actions.iter().copied());
    }
    if ad.usr2_requested {
        ad.usr2_requested = false;
        actions.extend(ad.sigusr2_actions.iter().copied());
    }
    actions
}

/// Moves each outbound connection to a new local UDP port, like a NAT
/// rebind. One socket each: quiche ties a path to its local address.
pub(super) fn migrate_client_connections(r: &mut Runner) {
    let (conns, _) = r.endpoint.mut_conns_and_app_data();
    let client_cids: Vec<_> = conns.iter_mut()
        .filter(|(_, conn)| !conn.conn.is_server())
        .map(|(cid, _)| cid)
        .collect();
    for cid in client_cids {
        let socket = match Socket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => { error!("migrate: bind new local socket for conn {cid}: {e}"); continue; }
        };
        let new_local_addr = socket.local_addr;
        r.register_socket(socket);
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        let Some(conn) = conns.get_mut(cid) else { continue };
        match conn.conn.migrate_source(new_local_addr) {
            Ok(_) => info!("conn {cid} migrating to new local address {new_local_addr}"),
            Err(e) => error!("migrate: conn {cid} migrate_source({new_local_addr}): {e:?}"),
        }
    }
}

/// An immediate ping on every connection.
pub(super) fn ping_all(r: &mut Runner) {
    let (conns, _) = r.endpoint.mut_conns_and_app_data();
    for (_, conn) in conns.iter_mut() {
        let paths: Vec<_> = conn.conn.path_stats().map(|p| (p.local_addr, p.peer_addr)).collect();
        for (local, peer) in paths {
            let _ = conn.conn.send_ack_eliciting_on_path(local, peer);
        }
    }
    info!("sent ping on all connections");
}

/// Every namespace `cid` announced is no longer routed to it.
fn forget_announcements(cid: ClientId, ad: &mut AppData) {
    ad.namespaces.retain(|_, owners| { owners.remove(&cid); !owners.is_empty() });
}

/// Forgets closed connections, and queues `--reconnect` re-dials.
pub(super) fn cleanup_closed_conns(r: &mut Runner, now: Instant) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        if !conn.conn.is_closed() { continue; }
        info!("conn {cid} disconnected");
        let reconnect_url = conn.app_data.reconnect_url.take();
        forget_announcements(cid, ad);
        for (nt, track) in ad.local_tracks.iter_mut() {
            let had_subscriptions = !track.subscriptions.is_empty();
            let partial: Vec<Location> = track.subscriptions.iter()
                .filter(|s| s.client_id == cid)
                .flat_map(|s| s.in_progress.iter().map(|&(loc, _)| loc))
                .collect();
            track.subscriptions.retain(|s| s.client_id != cid);
            // a disconnected #race candidate must not be raced or promoted
            let was_standby = track.race.as_mut().is_some_and(|race| {
                race.candidates.retain(|&c| c != cid);
                race.established.remove(&cid).is_some()
            });
            // lost its last source, unless a `#race` standby or a template
            // resubscribing after `--reconnect` takes over
            let resumable = track.has_race_standby()
                || reconnect_url.is_some() && ad.sub_track_templates.iter().any(|t| t.track == *nt);
            if (had_subscriptions || was_standby) && track.subscriptions.is_empty() && !resumable {
                for a in &mut track.aliases { a.publish_done = Some(PublishDoneStatus::TrackEnded); }
            }
            // nobody resends them, else they stay in progress forever
            // - an active race's forwarder fills those from its target on
            let racing_from = track.race.as_ref().and_then(|r| r.active.as_ref()).map(|a| a.target);
            if !(track.subscriptions.is_empty() && resumable) {
                for loc in partial.into_iter().filter(|&loc| racing_from.is_none_or(|target| loc < target)) {
                    cancel_abandoned(&mut track.cache, &track.subscriptions, &mut track.stats, loc);
                }
            }
            track.aliases.retain(|a| a.conn_id != cid);
        }
        ad.upstream_cids.remove(&cid);
        ad.client_active_pub_tracks.remove(&cid);
        if let Some(url) = reconnect_url {
            // right away, unless the last attempt did not even connect
            let delay = if conn.conn.is_established() { Duration::ZERO } else { RECONNECT_RETRY_INTERVAL };
            ad.pending_reconnects.push((now + delay, url));
        }
    }
}

/// Re-dials due `--reconnect`s, the time until the next one.
pub(super) fn redial(r: &mut Runner, now: Instant) -> Option<Duration> {
    let mut next = None;
    for (at, url) in std::mem::take(&mut r.endpoint.app_data_mut().pending_reconnects) {
        if at <= now {
            let (args, local_addr) = r.endpoint.app_data().reconnect.clone().unwrap();
            if let Err(e) = connect_upstream(r, &args, local_addr, &url) {
                error!("{e}, retrying in {RECONNECT_RETRY_INTERVAL:?}");
                next = earlier(next, RECONNECT_RETRY_INTERVAL);
                r.endpoint.app_data_mut().pending_reconnects.push((now + RECONNECT_RETRY_INTERVAL, url));
            }
        } else {
            next = earlier(next, at - now);
            r.endpoint.app_data_mut().pending_reconnects.push((at, url));
        }
    }
    next
}

/// Processes what each connection received.
pub(super) fn tick_connections(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        let is_server = conn.conn.is_server();
        // Correlation record: pairs this connection's qlog with its peer's.
        // - was derivable from `quic:packet_received`'s SCID, but that's Core
        //   importance (mixed in with high-volume packet/recovery events,
        //   the reason `--qlog-level moqt` exists) — one Application-importance
        //   event per connection instead, survives every qlog level
        // - not in `setup_qlog`: `destination_id()` is empty until handshake settles
        if !conn.app_data.logged_conn_ids && conn.conn.is_established() {
            conn.app_data.logged_conn_ids = true;
            let scid = format!("{:?}", conn.conn.source_id());
            let peer_scid = format!("{:?}", conn.conn.destination_id());
            if let Some(qlog) = conn.conn.qlog_streamer() {
                qlog.add_event_now(qlog::events::JsonEvent {
                    time: 0.0,
                    importance: qlog::events::EventImportance::default(),
                    name: "moq_cli:connection_ids".into(),
                    data: serde_json::json!({ "scid": scid, "peer_scid": peer_scid }),
                }).ok();
            }
        }
        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else {
            // Not ready for MoQ yet — normally momentary, but can stick here
            // forever (QUIC/WebTransport established, CLIENT_SETUP ACKed,
            // MoQ session never inits). Warn once instead of silently
            // showing up as a peer that transferred nothing.
            let now = Instant::now();
            let started = *conn.app_data.first_seen.get_or_insert(now);
            if !conn.app_data.warned_moq_stuck
                && conn.conn.is_established()
                && now.duration_since(started) > Duration::from_secs(2)
            {
                conn.app_data.warned_moq_stuck = true;
                error!(
                    "conn {cid} MoQ session still not initialized {:?} after first tick \
                     (quic established, is_server={is_server}, readable_streams={:?})",
                    now.duration_since(started),
                    conn.conn.readable().collect::<Vec<_>>(),
                );
            }
            continue;
        };
        conn.app_data.first_seen.get_or_insert_with(Instant::now);
        if let Some(v) = moq.session.version() && !conn.app_data.logged_connect {
            let peer = moq.t.quic().path_stats().next().map(|s| s.peer_addr).unwrap();
            let verb = if is_server { "accept" } else { "connected to" };
            info!("conn {cid} {verb} {peer} v{v}");
            conn.app_data.logged_connect = true;
        }
        if is_server && ad.listen_upstream && ad.upstream_cids.insert(cid) {
            sync_sub_track_templates(ad);
        }
        // Serving local tracks (--pub) and relay-forwarding (passive --sub
        // wildcards) don't depend on which side dialed the QUIC connection —
        // a plain outbound (--connect) publisher must accept/reject incoming
        // SUBSCRIBEs exactly like an accepted one.
        let upstream = ad.upstream_cids.contains(&cid);
        while let Some(event) = moq.session.next_event() {
            if let Event::RequestFailed { request_id, failure: error } = &event {
                conn.app_data.handles.forget(*request_id);
                let announced = &conn.app_data.announced_namespaces;
                if let Some((ns, _)) = announced.iter().find(|(_, a)| a.request_id() == *request_id) {
                    // kept, so it isn't announced again
                    info!("conn {cid} rejected namespace {ns}: {error}");
                    continue;
                }
            }
            let Some(event) = conn.app_data.responses.file(event) else { continue };
            let handles = &mut conn.app_data.handles;
            match event {
                Event::Request(IncomingRequest::RequestUpdate(r)) => answer_request_update(cid, &mut moq, ad, r),
                Event::Request(IncomingRequest::PublishNamespace(r)) => answer_publish_namespace(cid, &mut moq, ad, r, upstream),
                Event::Request(IncomingRequest::Subscribe(r)) if upstream => reject_from_upstream(cid, &mut moq, r),
                Event::Request(IncomingRequest::Subscribe(r)) => answer_subscribe(cid, &mut moq, handles, ad, r),
                Event::Request(IncomingRequest::Fetch(r)) if upstream => reject_from_upstream(cid, &mut moq, r),
                Event::Request(IncomingRequest::Fetch(r)) => ad.fetches.push(DownstreamFetch::new(cid, r)),
                Event::Request(IncomingRequest::Publish(r)) if upstream => reject_from_upstream(cid, &mut moq, r),
                Event::Request(IncomingRequest::Publish(r)) => answer_publish(cid, &mut moq, handles, ad, r),
                Event::RequestCancelled(request_id) => drop_withdrawn_subscribe(cid, request_id, ad),
                Event::NamespaceWithdrawn { namespace, .. } => forget_announcer(cid, &namespace, ad),
                Event::Request(IncomingRequest::SubscribeNamespace(r)) => answer_subscribe_namespace(cid, &mut moq, handles, r),
                Event::Request(IncomingRequest::SubscribeTracks(r)) => answer_subscribe_tracks(cid, &mut moq, handles, r),
                Event::PrefixSubscriptionEnded { request_id } => handles.forget(request_id),
                Event::Unsubscribed { request_id, track_alias } => {
                    handles.forget(request_id);
                    apply_unsubscribe(cid, track_alias, ad);
                }
                Event::SubscriptionUpdated { request_id, forward, filter } => apply_subscription_update(cid, request_id, forward, filter, ad),
                _ => {}
            }
        }
        // the connection's only session: its subscriptions end with it in `cleanup_closed_conns`,
        // its namespaces now, no SUBSCRIBE may be routed to it meanwhile
        if moq.session.is_closed() && !conn.conn.is_draining() {
            info!("conn {cid} session closed, closing the connection");
            forget_announcements(cid, ad);
            let _ = conn.conn.close(true, quiche::h3::WireErrorCode::NoError as u64, b"");
        }
    }
}

/// Starts or stops the padding stream, sent by the next `send_padding`.
pub(super) fn toggle_padding_streams(r: &mut Runner) {
    let connect_only = r.endpoint.app_data().padding_stream_connect_only;
    let (conns, _) = r.endpoint.mut_conns_and_app_data();
    for (_, conn) in conns.iter_mut() {
        if connect_only && conn.conn.is_server() { continue; }
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        if moq.session.padding_stream_running() {
            moq.session.stop_padding_stream(&mut moq.t);
        } else {
            moq.session.start_padding_stream();
        }
    }
    info!(
        "toggled padding stream on {} connections",
        if connect_only { "--connect/--upstream" } else { "all" }
    );
}

/// PUBLISH_NAMESPACE_DONE for each of `targets` a connection announced.
pub(super) fn withdraw_namespaces(r: &mut Runner, targets: &[Namespace]) {
    let (conns, _) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        for ns in targets {
            if let Some(announcement) = conn.app_data.announced_namespaces.remove(ns) {
                moq.session.publish_namespace_done(announcement, &mut moq.t);
                info!("conn {cid} withdrew namespace {ns}");
            }
        }
    }
}

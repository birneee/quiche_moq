mod args;

use std::collections::HashMap;
use boring::ssl::{SslContextBuilder, SslMethod};
use log::{LevelFilter, error, info};
use quiche_mio_runner as runner;
use quiche_mio_runner::Socket;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig, quiche, ClientId};
use quiche_moq as moq;
use quiche_moq::{SubscriptionRequestAction};
use quiche_moq::wire::{KeyValuePairs, Location, Namespace, NamespaceTrackname, REQUEST_ERROR_DOES_NOT_EXIST, RequestId, TrackAlias, version_to_name};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use quiche_utils::cert::load_or_generate_keys;
use url::Url;
use clap::Parser;

use crate::args::Args;

type Runner = runner::Runner<ConnAppData, AppData, ()>;

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
    /// Namespaces announced to this connection: namespace → request_id used in publish_namespace.
    announced_namespaces: HashMap<Namespace, RequestId>,
    logged_connect: bool,
}

struct SubscriberInfo {
    client_id: ClientId,
    /// Subscriber's request_id; kept for the lifetime of the subscription.
    request_id: RequestId,
    /// Set once relay sends SUBSCRIBE_OK to subscriber (Phase 4.5).
    track_alias: Option<TrackAlias>,
    /// Set when the publisher disconnects; triggers PUBLISH_DONE in Phase 4.6.
    publisher_gone: bool,
    /// Location (group+object) of the most recent object header forwarded to this subscriber.
    /// None before any header has been sent.
    /// A value < Subscription::location means the subscriber is behind and needs a new header.
    location: Option<Location>,
    /// Bytes of the current object's payload still to be forwarded to this subscriber.
    /// Offset into obj_buf = obj_payload_len - remaining.
    /// 0 when fully forwarded or skipping this object (effectively read_pos = end of object).
    remaining: usize,
}

impl SubscriberInfo {
    fn is_accepted(&self) -> bool { self.track_alias.is_some() }
}

struct PublisherInfo {
    client_id: ClientId,
    /// Request ID for the relay's SUBSCRIBE sent to the publisher (set in Phase 3).
    request_id: Option<RequestId>,
    /// Track alias from publisher's SUBSCRIBE_OK (set in Phase 4).
    track_alias: Option<TrackAlias>,
    /// Largest location from publisher's SUBSCRIBE_OK, forwarded to subscribers.
    largest_location: Option<Location>,
}

struct Subscription {
    /// Publisher state; None when publisher has disconnected.
    publisher: Option<PublisherInfo>,
    subscribers: Vec<SubscriberInfo>,
    /// Payload length of the current object (0 when no object received yet).
    obj_payload_len: usize,
    /// Bytes written into obj_buf from the publisher for the current object.
    write_pos: usize,
    /// Buffer holding the full payload of the current object.
    obj_buf: Vec<u8>,
    /// Location (group+object) of the current object being buffered from the publisher.
    /// None until the first object header is received.
    location: Option<Location>,
    /// Extension headers of the current object, forwarded to subscribers on the next object header.
    ext_hdrs: KeyValuePairs,
    /// Set when read_obj_pld returns Fin mid-object (publisher reset the subgroup stream).
    /// Checked at the start of the next loop iteration before any forwarding occurs.
    subgroup_canceled: bool,
}

impl Subscription {
    fn is_sent(&self) -> bool {
        self.publisher.as_ref().is_some_and(|p| p.request_id.is_some())
    }

    fn is_publisher_accepted(&self) -> bool {
        self.publisher.as_ref().is_some_and(|p| p.track_alias.is_some())
    }

    fn has_accepted_subscribers(&self) -> bool {
        self.subscribers.iter().any(|s| s.track_alias.is_some())
    }
}

struct AppData {
    namespaces: HashMap<Namespace, ClientId>,
    subscriptions: HashMap<NamespaceTrackname, Subscription>,
}

#[allow(clippy::field_reassign_with_default)]
fn main() {
    env_logger::builder()
        .filter_level(LevelFilter::Info)
        .parse_default_env()
        .init();
    let args = Args::parse();
    let socket = Socket::bind(format!("0.0.0.0:{}", args.port)).unwrap();
    let local_addr = socket.local_addr;
    info!("relay listening on {}", local_addr);
    let (cert, key) = load_or_generate_keys(&None, &None);
    let mut r = Runner::new(
        {
            let mut c = runner::Config::<ConnAppData, AppData, ()>::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        Endpoint::new(
            Some({
                let mut c = ServerConfig::new(|_| ConnAppData {
                    moq_helper: MoqWebTransportHelper::new_server(moq::Config::default()),
                    announced_namespaces: HashMap::new(),
                    logged_connect: false,
                });
                c.client_config = {
                    let mut c =
                        quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, {
                            let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                            b.set_private_key(&key).unwrap();
                            b.set_certificate(&cert).unwrap();
                            b
                        })
                        .unwrap();
                    MoqWebTransportHelper::configure_quic(&mut c);
                    c.set_max_idle_timeout(args.timeout);
                    c
                };
                c
            }),
            {
                let mut c = EndpointConfig::default();
                c.setup_qlog = quiche_endpoint_utils::setup_qlog;
                c
            },
            AppData {
                namespaces: Default::default(),
                subscriptions: Default::default(),
            },
        ),
        None,
    );
    r.register_socket(socket);
    if let Some(relay_url) = &args.relay {
        let url = Url::parse(relay_url).unwrap();
        let peer_addr = *url.socket_addrs(|| Some(443)).unwrap().first().unwrap();
        let mut quic_cfg = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
        MoqWebTransportHelper::configure_quic(&mut quic_cfg);
        quic_cfg.verify_peer(false);
        quic_cfg.set_max_idle_timeout(args.timeout);
        r.endpoint.connect(
            None, local_addr, peer_addr, &mut quic_cfg,
            ConnAppData {
                moq_helper: MoqWebTransportHelper::new_client(url.clone(), moq::Config::default()),
                announced_namespaces: HashMap::new(),
                logged_connect: false,
            },
            None, None,
        );
        info!("connecting to relay {}", relay_url);
    }
    r.run();
}

fn post_handle_recvs(r: &mut Runner) {
    // Phase 0: Detect closed connections and clean up state.
    // Namespace un-announcement happens in Phase 2; subscriber notifications in Phase 4.6.
    let (conns, appdata) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        if conn.conn.is_closed() {
            info!("Client {} disconnected", cid);
            appdata.namespaces.retain(|_, owner| *owner != cid);
            for sub in appdata.subscriptions.values_mut() {
                if sub.publisher.as_ref().is_some_and(|p| p.client_id == cid) {
                    sub.publisher = None;
                    for s in &mut sub.subscribers {
                        s.publisher_gone = true;
                    }
                }
                sub.subscribers.retain(|s| s.client_id != cid);
            }
        }
    }

    // Phase 1: Per-connection processing (receive subscriptions, namespace publishes)
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (icid, conn) in conns.iter_mut() {
        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        post_handle_recvs_conn(
            icid,
            moq,
            appdata,
            &mut conn.app_data.logged_connect,
        );
    }

    // Phase 2: Un-announce gone namespaces and announce new ones to all connected MoQ clients.
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (icid, conn) in conns.iter_mut() {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        conn.app_data.announced_namespaces.retain(|ns, rid| {
            if appdata.namespaces.contains_key(ns) { true } else { moq.publish_namespace_done(*rid); false }
        });
        for (ns, &publisher) in appdata.namespaces.iter() {
            if publisher == icid { continue; }
            if conn.app_data.announced_namespaces.contains_key(ns) { continue; }
            match moq.publish_namespace(ns.0.0.clone()) {
                Ok(request_id) => {
                    info!("announced namespace {} to {}", ns, icid);
                    conn.app_data.announced_namespaces.insert(ns.clone(), request_id);
                }
                Err(e) => {
                    error!("failed to announce namespace {} to {}: {:?}", ns, icid, e);
                }
            }
        }
    }

    // Phase 3: Forward pending subscriptions to publishers
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (nt, sub) in appdata.subscriptions.iter_mut() {
        if sub.is_sent() { continue; }
        // Populate publisher from current namespace map if not set (e.g. after reconnect)
        if sub.publisher.is_none() {
            let Some(&pub_id) = appdata.namespaces.get(nt.namespace()) else { continue };
            sub.publisher = Some(PublisherInfo { client_id: pub_id, request_id: None, track_alias: None, largest_location: None });
        }
        let Some(pub_info) = sub.publisher.as_mut() else { continue };
        let Some(conn) = conns.get_mut(pub_info.client_id) else { continue };
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        match moq.subscribe(nt) {
            Ok(request_id) => {
                pub_info.request_id = Some(request_id);
                info!("sent subscription request {} to {}", nt, pub_info.client_id);
            }
            Err(e) => {
                error!("failed to subscribe {} on publisher {}: {:?}", nt, pub_info.client_id, e);
            }
        }
    }

    // Phase 4: Poll subscribe responses from publishers
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    appdata.subscriptions.retain(|nt, sub| {
        let Some(pub_info) = sub.publisher.as_mut() else { return true };
        let Some(request_id) = pub_info.request_id else { return true };
        if pub_info.track_alias.is_some() { return true; }
        let Some(conn) = conns.get_mut(pub_info.client_id) else { return true };
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { return true };
        let Some(response) = moq.poll_subscribe_response(request_id) else { return true };
        match response {
            Ok((track_alias, ok_msg)) => {
                pub_info.track_alias = Some(track_alias);
                pub_info.largest_location = ok_msg.largest_location();
                info!("accepted track {} by {}", nt, pub_info.client_id);
                true
            }
            Err(e) => {
                error!("publisher rejected {} with {} - {}", nt, e.error_code(), e.error_reason());
                false
            }
        }
    });

    // Phase 4.5: Accept pending subscribers now that publisher has accepted
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for sub in appdata.subscriptions.values_mut() {
        if !sub.is_publisher_accepted() { continue; }
        let largest_location = sub.publisher.as_ref().and_then(|p| p.largest_location);
        for s in &mut sub.subscribers {
            if s.is_accepted() { continue; }
            if let Some(sub_conn) = conns.get_mut(s.client_id)
                && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn) {
                    s.track_alias = Some(moq.accept_subscription(s.request_id, largest_location));
                    info!("accept track for {}", s.client_id)
                }
        }
    }

    // Phase 4.6: Send PUBLISH_DONE to subscribers whose publisher disconnected, then remove them.
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for sub in appdata.subscriptions.values_mut() {
        sub.subscribers.retain(|s| {
            if !s.publisher_gone { return true; }
            if let Some(sub_conn) = conns.get_mut(s.client_id)
                && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
            {
                moq.publish_done(s.request_id);
            }
            false
        });
    }

    // Phase 5: Forward object data from publishers to subscribers.
    //
    // obj_buf holds the payload of the current object as it arrives from the publisher.
    // write_pos: bytes written into obj_buf so far by the publisher.
    // Subscriber state (per-subscriber):
    //   location: group+object of the most recent header sent to this subscriber (None = not started).
    //   remaining: bytes of the current object's payload still to forward.
    //              read_pos into obj_buf = obj_payload_len - remaining.
    //              0 means fully forwarded or skipping this object (read_pos = end).
    //
    // Loop steps per iteration:
    //   1. Cancel check: if the publisher's subgroup was reset mid-object, the partial payload
    //      is incomplete. Discard it and reset streams for subscribers that had unforwarded data.
    //   2. Forward: for each subscriber whose location is behind sub.location, send the current
    //      object header first (resetting any unfinished previous stream), then forward
    //      obj_buf[read_pos..write_pos].
    //   3. Read: consume more payload or the next object header from the publisher.
    //      Stores the new location and ext_hdrs in sub; subscribers pick them up in step 2.
    //   4. Break when the publisher made no progress.
    //
    // The publisher is never blocked by slow subscribers. A subscriber that cannot receive
    // right now retains its remaining count and catches up on later post_handle_recvs calls.
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (nt, sub) in appdata.subscriptions.iter_mut() {
        let Some((pub_id, pub_ta)) = sub.publisher.as_ref().and_then(|p| p.track_alias.map(|ta| (p.client_id, ta))) else { continue };
        if !sub.has_accepted_subscribers() { continue; }

        loop {
            // Step 1: If the publisher's subgroup stream was reset mid-object, the partial payload
            // is incomplete. Clear the buffer and reset streams for subscribers that still had
            // unforwarded data. Those subscribers keep their location = sub.location so they
            // appear "done" with this canceled object and will receive the next header normally.
            if sub.subgroup_canceled {
                sub.subgroup_canceled = false;
                sub.write_pos = 0;
                sub.obj_payload_len = 0;
                for s in sub.subscribers.iter_mut() {
                    if s.location != sub.location || s.remaining == 0 { continue; }
                    let Some(sub_ta) = s.track_alias else { s.remaining = 0; continue };
                    if let Some(sub_conn) = conns.get_mut(s.client_id)
                        && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
                    {
                        error!("reset stream for subscriber {} on {}: upstream subgroup canceled", s.client_id, nt);
                        moq.reset_current_track_stream(sub_ta);
                    }
                    s.remaining = 0;
                }
            }

            // Step 2: Forward to each subscriber.
            // If the subscriber's location is behind sub.location (or None), send the current
            // object header first, resetting the stream if there was unforwarded data.
            // Then forward obj_buf[read_pos..write_pos] where read_pos = obj_payload_len - remaining.
            if let Some(sub_loc) = sub.location {
                for s in sub.subscribers.iter_mut() {
                    let need_header = s.location.is_none_or(|loc| loc < sub_loc);
                    let Some(sub_ta) = s.track_alias else { continue };
                    if let Some(sub_conn) = conns.get_mut(s.client_id)
                        && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
                    {
                        if need_header {
                            if s.remaining != 0 {
                                error!("reset stream for subscriber {} on {}: subscriber is behind, skipping to {:?}", s.client_id, nt, sub_loc);
                                moq.reset_current_track_stream(sub_ta);
                                s.remaining = 0;
                            }
                            match moq.send_obj_hdr_with(Some(sub_loc.group), None, Some(sub_loc.object), sub.obj_payload_len, &sub.ext_hdrs, sub_ta) {
                                Ok(()) => {
                                    s.location = Some(sub_loc);
                                    s.remaining = sub.obj_payload_len;
                                }
                                Err(moq::Error::InsufficientCapacity) => continue,
                                Err(e) => unimplemented!("{:?}", e),
                            }
                        }
                        let read_pos = sub.obj_payload_len - s.remaining;
                        if read_pos >= sub.write_pos { continue; }
                        match moq.send_obj_pld(&sub.obj_buf[read_pos..sub.write_pos], sub_ta) {
                            Ok(n) => { s.remaining -= n; }
                            Err(moq::Error::Done | moq::Error::InsufficientCapacity) => {}
                            Err(e) => unimplemented!("{:?}", e),
                        }
                    }
                }
            }

            // Step 3: Consume data from the publisher — more payload or the next object header.
            // write_pos < obj_payload_len  → read more payload.
            // write_pos >= obj_payload_len → object complete (or none yet); read next header.
            // On new header: location, obj_payload_len, write_pos, obj_buf, and ext_hdrs are updated.
            // Subscribers will detect the new location in step 2 of the next iteration.
            let mut pub_progress = false;
            let mut publisher_fin = false;
            if let Some(pub_conn) = conns.get_mut(pub_id)
                && let Some(mut moq) = pub_conn.app_data.moq_helper.moq_handle(&mut pub_conn.conn)
            {
                if sub.write_pos < sub.obj_payload_len {
                    match moq.read_obj_pld(&mut sub.obj_buf[sub.write_pos..], pub_ta) {
                        Ok(n) => { sub.write_pos += n; pub_progress = true; }
                        Err(moq::Error::Done) => {}
                        Err(moq::Error::Fin) => {
                            error!("publisher subgroup reset mid-object for {}", nt);
                            sub.subgroup_canceled = true;
                            pub_progress = true; // step 1 runs next iteration
                        }
                        Err(e) => { error!("read obj pld for {}: {:?}", nt, e); publisher_fin = true; }
                    }
                } else {
                    match moq.read_obj_hdr(pub_ta) {
                        Ok(hdr) => {
                            let group = moq.subgroup_header(pub_ta)
                                .map_or_else(|| sub.location.map_or(0, |l| l.group), |sg| sg.group_id());
                            sub.location = Some(Location { group, object: hdr.id() });
                            sub.obj_payload_len = hdr.payload_len();
                            sub.write_pos = 0;
                            sub.obj_buf.resize(hdr.payload_len(), 0);
                            sub.ext_hdrs = hdr.extension_headers().clone();
                            pub_progress = true;
                        }
                        Err(moq::Error::Done) => {}
                        Err(moq::Error::Fin) => { publisher_fin = true; }
                        Err(e) => { error!("read obj hdr for {}: {:?}", nt, e); publisher_fin = true; }
                    }
                }
            }

            if publisher_fin {
                info!("publisher done for {}", nt);
                sub.publisher = None;
                for s in &mut sub.subscribers { s.publisher_gone = true; }
                break;
            }

            // Step 4: Continue only while the publisher makes progress.
            if !pub_progress { break; }
        }
    }
}

fn post_handle_recvs_conn(
    cid: ClientId,
    mut moq: MoqHandle<'_>,
    app_data: &mut AppData,
    logged_connect: &mut bool,
) {
    // log connection
    if let Some(version) = moq.version() && !*logged_connect {
        let peer_addr = moq.quic().path_stats().next().map(|s| s.peer_addr).unwrap();
        info!("Client {cid} connected {peer_addr:?} v{}", version_to_name(version));
        *logged_connect = true;
    }

    moq.process_subscription_requests(|request_id, cm| {
        let nt = &cm.namespace_trackname;
        // Skip subscriptions already queued from a previous frame.
        if app_data.subscriptions.get(nt)
            .map(|sub| sub.subscribers.iter().any(|s| !s.is_accepted() && s.request_id == *request_id))
            .unwrap_or(false)
        {
            return SubscriptionRequestAction::Keep;
        }
        if let Some(&publisher_id) = app_data.namespaces.get(nt.namespace()) {
            let nt = nt.clone();
            let sub = app_data.subscriptions.entry(nt.clone()).or_insert_with(|| {
                info!("new subscription request {} from {} (publisher: {})", nt, cid, publisher_id);
                Subscription {
                    publisher: Some(PublisherInfo {
                        client_id: publisher_id,
                        request_id: None,
                        track_alias: None,
                        largest_location: None,
                    }),
                    subscribers: Vec::new(),
                    obj_payload_len: 0,
                    write_pos: 0,
                    obj_buf: Vec::new(),
                    location: None,
                    ext_hdrs: KeyValuePairs::new(),
                    subgroup_canceled: false,
                }
            });
            if !sub.is_publisher_accepted() {
                info!("queued subscriber {} for {} (awaiting publisher accept)", cid, nt);
            }
            sub.subscribers.push(SubscriberInfo { client_id: cid, request_id: *request_id, track_alias: None, publisher_gone: false, location: None, remaining: 0 });
            SubscriptionRequestAction::Keep
        } else {
            info!("reject subscription {} from {} (no publisher)", nt, cid);
            SubscriptionRequestAction::Reject(REQUEST_ERROR_DOES_NOT_EXIST)
        }
    });
    while let Some((&request_id, cm)) = moq.next_pending_namespace_publish() {
        let namespace = cm.track_namespace().clone();
        info!("accept namespace {} from {}", namespace, cid);
        app_data.namespaces.insert(namespace, cid);
        moq.accept_namespace_publish(request_id);
    }
}

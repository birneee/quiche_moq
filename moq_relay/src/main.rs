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
    /// Payload length of the object currently being relayed (0 = no object in progress).
    obj_payload_len: usize,
    /// Payload bytes already forwarded to subscribers for the current object.
    obj_forwarded: usize,
    /// Scratch buffer for reading payload from publisher.
    obj_buf: Vec<u8>,
    /// Group ID of the current object (from publisher's subgroup header).
    obj_group_id: u64,
    /// Object ID of the current object (from publisher's object header).
    obj_object_id: u64,
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
                    c
                };
                c
            }),
            EndpointConfig::default(),
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
        quic_cfg.verify_peer(false);
        MoqWebTransportHelper::configure_quic(&mut quic_cfg);
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

    // Phase 5: Forward object data from publishers to subscribers (streaming)
    enum FwdStep { Hdr(usize, KeyValuePairs), Pld(usize), Stop, Fin }
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (nt, sub) in appdata.subscriptions.iter_mut() {
        let Some((pub_id, pub_ta)) = sub.publisher.as_ref().and_then(|p| p.track_alias.map(|ta| (p.client_id, ta))) else { continue };
        if !sub.has_accepted_subscribers() { continue; }

        loop {
            // Read one step from publisher; release borrow before forwarding.
            let step = if let Some(pub_conn) = conns.get_mut(pub_id) {
                if let Some(mut moq) = pub_conn.app_data.moq_helper.moq_handle(&mut pub_conn.conn) {
                    if sub.obj_payload_len == 0 {
                        match moq.read_obj_hdr(pub_ta) {
                            Ok(hdr) => {
                                sub.obj_payload_len = hdr.payload_len();
                                sub.obj_forwarded = 0;
                                sub.obj_object_id = hdr.id();
                                if let Some(sg) = moq.subgroup_header(pub_ta) {
                                    sub.obj_group_id = sg.group_id();
                                }
                                FwdStep::Hdr(sub.obj_payload_len, hdr.extension_headers().clone())
                            }
                            Err(moq::Error::Done) => FwdStep::Stop,
                            Err(moq::Error::Fin) => FwdStep::Fin,
                            Err(e) => { error!("read obj hdr for {}: {:?}", nt, e); FwdStep::Stop }
                        }
                    } else {
                        let remaining = sub.obj_payload_len - sub.obj_forwarded;
                        sub.obj_buf.resize(remaining, 0);
                        match moq.read_obj_pld(&mut sub.obj_buf, pub_ta) {
                            Ok(n) => FwdStep::Pld(n),
                            Err(moq::Error::Done) => FwdStep::Stop,
                            Err(e) => { error!("read obj pld for {}: {:?}", nt, e); FwdStep::Stop }
                        }
                    }
                } else { FwdStep::Stop }
            } else { FwdStep::Stop };

            // Forward to subscribers (publisher borrow released above).
            match step {
                FwdStep::Fin => {
                    info!("publisher done for {}", nt);
                    sub.publisher = None;
                    for s in &mut sub.subscribers { s.publisher_gone = true; }
                    break;
                }
                FwdStep::Stop => break,
                FwdStep::Hdr(len, ext_hdrs) => {
                    for s in &sub.subscribers {
                        let Some(sub_ta) = s.track_alias else { continue };
                        if let Some(sub_conn) = conns.get_mut(s.client_id)
                            && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
                                && let Err(e) = moq.send_obj_hdr_with(Some(sub.obj_group_id), None, Some(sub.obj_object_id), len, &ext_hdrs, sub_ta) {
                                    error!("send obj hdr to subscriber {} for {}: {:?}", s.client_id, nt, e);
                                }
                    }
                }
                FwdStep::Pld(n) => {
                    sub.obj_forwarded += n;
                    for s in &sub.subscribers {
                        let Some(sub_ta) = s.track_alias else { continue };
                        if let Some(sub_conn) = conns.get_mut(s.client_id)
                            && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
                                && let Err(e) = moq.send_obj_pld(&sub.obj_buf[..n], sub_ta) {
                                    error!("send obj pld to subscriber {} for {}: {:?}", s.client_id, nt, e);
                                }
                    }
                    if sub.obj_forwarded >= sub.obj_payload_len {
                        sub.obj_payload_len = 0;
                    }
                }
            }
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
                    obj_forwarded: 0,
                    obj_buf: Vec::new(),
                    obj_group_id: 0,
                    obj_object_id: 0,
                }
            });
            if !sub.is_publisher_accepted() {
                info!("queued subscriber {} for {} (awaiting publisher accept)", cid, nt);
            }
            sub.subscribers.push(SubscriberInfo { client_id: cid, request_id: *request_id, track_alias: None, publisher_gone: false });
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

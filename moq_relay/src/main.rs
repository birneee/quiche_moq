use std::collections::{HashMap, HashSet};
use boring::ssl::{SslContextBuilder, SslMethod};
use log::{LevelFilter, error, info};
use quiche_mio_runner as runner;
use quiche_mio_runner::Socket;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig, quiche, ClientId};
use quiche_moq as moq;
use quiche_moq::wire::{Namespace, NamespaceTrackname, RequestId, TrackAlias, REQUEST_ERROR_DOES_NOT_EXIST};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use quiche_utils::cert::load_or_generate_keys;

type Runner = runner::Runner<ConnAppData, AppData, ()>;

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
    announced_namespaces: HashSet<Namespace>,
}

struct SubscriberInfo {
    client_id: ClientId,
    /// Subscriber's request_id held until publisher accepts; cleared once accepted.
    pending_request_id: Option<RequestId>,
    /// Set once publisher accepts and relay sends SUBSCRIBE_OK to subscriber.
    track_alias: Option<TrackAlias>,
}

struct Subscription {
    /// The relay's own request_id used when subscribing to the publisher.
    relay_request_id: Option<RequestId>,
    /// Track alias from publisher's SUBSCRIBE_OK, for reading data.
    publisher_track_alias: Option<TrackAlias>,
    /// Publisher connection id.
    publisher_id: ClientId,
    subscribers: Vec<SubscriberInfo>,
    /// Payload length of the object currently being relayed (0 = no object in progress).
    obj_payload_len: usize,
    /// Payload bytes already forwarded to subscribers for the current object.
    obj_forwarded: usize,
    /// Scratch buffer for reading payload from publisher.
    obj_buf: Vec<u8>,
}

impl Subscription {
    fn is_sent(&self) -> bool {
        self.relay_request_id.is_some()
    }

    fn is_publisher_accepted(&self) -> bool {
        self.publisher_track_alias.is_some()
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
    let socket = Socket::bind("0.0.0.0:8080").unwrap();
    info!("relay listening on {}", socket.local_addr);
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
                    announced_namespaces: HashSet::new(),
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
    r.run();
}

fn post_handle_recvs(r: &mut Runner) {
    // Phase 1: Per-connection processing (receive subscriptions, namespace publishes)
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (icid, conn) in conns.iter_mut() {
        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        post_handle_recvs_conn(icid, moq, appdata);
    }

    // Phase 2: Announce known namespaces to all connected Moq clients
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (icid, conn) in conns.iter_mut() {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        for (ns, &publisher) in appdata.namespaces.iter() {
            if publisher == icid { continue; }
            if conn.app_data.announced_namespaces.contains(ns) { continue; }
            match moq.publish_namespace(ns.0.0.clone()) {
                Ok(()) => {
                    info!("announced namespace {} to {}", ns, icid);
                    conn.app_data.announced_namespaces.insert(ns.clone());
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
        let Some(&publisher) = appdata.namespaces.get(nt.namespace()) else { continue };
        let Some(conn) = conns.get_mut(publisher) else { continue };
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        match moq.subscribe(nt) {
            Ok(request_id) => {
                sub.relay_request_id = Some(request_id);
                info!("sent subscription {} to {}", nt, publisher);
            }
            Err(e) => {
                error!("failed to subscribe {} on publisher {}: {:?}", nt, publisher, e);
            }
        }
    }

    // Phase 4: Poll subscribe responses from publishers
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    appdata.subscriptions.retain(|nt, sub| {
        let Some(relay_request_id) = sub.relay_request_id else { return true };
        if sub.publisher_track_alias.is_some() { return true; }
        let Some(conn) = conns.get_mut(sub.publisher_id) else { return true };
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { return true };
        let Some(response) = moq.poll_subscribe_response(relay_request_id) else { return true };
        match response {
            Ok((track_alias, _)) => {
                sub.publisher_track_alias = Some(track_alias);
                info!("publisher accepted {}", nt);
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
        for s in &mut sub.subscribers {
            let Some(pending_rid) = s.pending_request_id.take() else { continue };
            if let Some(sub_conn) = conns.get_mut(s.client_id)
                && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn) {
                    s.track_alias = Some(moq.accept_subscription(pending_rid));
                }
        }
    }

    // Phase 5: Forward object data from publishers to subscribers (streaming)
    enum FwdStep { Hdr(usize), Pld(usize), Stop }
    let (conns, appdata) = &mut r.endpoint.mut_conns_and_app_data();
    for (nt, sub) in appdata.subscriptions.iter_mut() {
        let Some(pub_ta) = sub.publisher_track_alias else { continue };
        if !sub.has_accepted_subscribers() { continue; }

        loop {
            // Read one step from publisher; release borrow before forwarding.
            let step = if let Some(pub_conn) = conns.get_mut(sub.publisher_id) {
                if let Some(mut moq) = pub_conn.app_data.moq_helper.moq_handle(&mut pub_conn.conn) {
                    if sub.obj_payload_len == 0 {
                        match moq.read_obj_hdr(pub_ta) {
                            Ok(hdr) => {
                                sub.obj_payload_len = hdr.payload_len();
                                sub.obj_forwarded = 0;
                                FwdStep::Hdr(sub.obj_payload_len)
                            }
                            Err(moq::Error::Done) => FwdStep::Stop,
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
                FwdStep::Stop => break,
                FwdStep::Hdr(len) => {
                    for s in &sub.subscribers {
                        let Some(sub_ta) = s.track_alias else { continue };
                        if let Some(sub_conn) = conns.get_mut(s.client_id)
                            && let Some(mut moq) = sub_conn.app_data.moq_helper.moq_handle(&mut sub_conn.conn)
                                && let Err(e) = moq.send_obj_hdr(len, sub_ta) {
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
) {
    // Iterate the inbox; defer moq mutations to after the shared borrow ends.
    let mut to_reject: Vec<RequestId> = Vec::new();
    let mut to_accept_now: Vec<(NamespaceTrackname, RequestId)> = Vec::new();
    for (&request_id, cm) in moq.pending_received_subscriptions() {
        let nt = &cm.namespace_trackname;
        // Skip subscriptions already queued from a previous frame.
        if app_data.subscriptions.get(nt)
            .map(|sub| sub.subscribers.iter().any(|s| s.pending_request_id == Some(request_id)))
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(&publisher_id) = app_data.namespaces.get(nt.namespace()) {
            let nt = nt.clone();
            let sub = app_data.subscriptions.entry(nt.clone()).or_insert_with(|| {
                info!("new subscription {} from {} (publisher: {})", nt, cid, publisher_id);
                Subscription {
                    relay_request_id: None,
                    publisher_track_alias: None,
                    publisher_id,
                    subscribers: Vec::new(),
                    obj_payload_len: 0,
                    obj_forwarded: 0,
                    obj_buf: Vec::new(),
                }
            });
            if sub.is_publisher_accepted() {
                to_accept_now.push((nt, request_id));
            } else {
                info!("queued subscriber {} for {} (awaiting publisher accept)", cid, nt);
                sub.subscribers.push(SubscriberInfo { client_id: cid, pending_request_id: Some(request_id), track_alias: None });
            }
        } else {
            info!("reject subscription {} from {} (no publisher)", nt, cid);
            to_reject.push(request_id);
        }
    }
    for request_id in to_reject {
        moq.reject_subscription(request_id, REQUEST_ERROR_DOES_NOT_EXIST);
    }
    for (nt, request_id) in to_accept_now {
        let track_alias = moq.accept_subscription(request_id);
        if let Some(sub) = app_data.subscriptions.get_mut(&nt) {
            info!("accepted subscriber {} for {}", cid, nt);
            sub.subscribers.push(SubscriberInfo { client_id: cid, pending_request_id: None, track_alias: Some(track_alias) });
        }
    }
    while let Some((&request_id, cm)) = moq.next_pending_namespace_publish() {
        let namespace = cm.track_namespace().clone();
        info!("accept publish namespace: {}", namespace);
        app_data.namespaces.insert(namespace, cid);
        moq.accept_namespace_publish(request_id);
    }
}

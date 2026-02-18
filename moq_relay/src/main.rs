use std::collections::HashMap;
use boring::ssl::{SslContextBuilder, SslMethod};
use log::{LevelFilter, error, info};
use quiche_mio_runner as runner;
use quiche_mio_runner::Socket;
use quiche_mio_runner::quiche_endpoint::quiche::h3;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig, quiche, ClientId};
use quiche_moq as moq;
use quiche_moq::wire::{Namespace, NamespaceTrackname, RequestId, TrackAlias, REQUEST_ERROR_DOES_NOT_EXIST};
use quiche_moq_webtransport_helper::{MoqWebTransportHelper, State};
use quiche_utils::cert::load_or_generate_keys;
use quiche_webtransport as wt;

type Runner = runner::Runner<ConnAppData, AppData, ()>;

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
}

#[allow(dead_code)]
struct SubscriberInfo {
    client_id: ClientId,
    request_id: RequestId,
    /// Set after relay accepts the subscription on the subscriber side.
    track_alias: Option<TrackAlias>,
}

struct Subscription {
    sent: bool,
    /// The relay's own request_id used when subscribing to the publisher.
    relay_request_id: Option<RequestId>,
    /// Track alias from publisher's SUBSCRIBE_OK, for reading data.
    publisher_track_alias: Option<TrackAlias>,
    /// Publisher connection id.
    publisher_id: Option<ClientId>,
    subscribers: Vec<SubscriberInfo>,
    /// Payload length of the object currently being relayed (0 = no object in progress).
    obj_payload_len: usize,
    /// Accumulated payload bytes for the object currently being relayed.
    obj_buf: Vec<u8>,
}

struct AppData {
    namespaces: HashMap<Namespace, ClientId>,
    subscriptions: HashMap<NamespaceTrackname, Subscription>,
    /// Map relay's outgoing request_id to NamespaceTrackname for correlating publisher responses.
    relay_requests: HashMap<RequestId, NamespaceTrackname>,
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
                relay_requests: Default::default(),
            },
        ),
        None,
    );
    r.register_socket(socket);
    r.run();
}

fn post_handle_recvs(r: &mut Runner) {
    // Phase 1: Per-connection processing (receive subscriptions, namespace publishes)
    for icid in &mut r.endpoint.conn_index_iter() {
        let (Some(conn), appdata) = r.endpoint.conn_with_app_data_mut(icid) else {
            continue;
        };
        let quic_conn = &mut conn.conn;
        conn.app_data.moq_helper.on_post_handle_recvs(quic_conn);
        match &mut conn.app_data.moq_helper.state {
            State::Quic => {
                if quic_conn.is_timed_out() {
                    error!("client timed out")
                }
                if quic_conn.is_closed() {
                    error!("client closed")
                }
            }
            State::H3 { .. } => {}
            State::Wt { .. } => {}
            State::MoqHandshake { .. } => {}
            State::Moq {
                wt_conn,
                h3_conn,
                moq_session,
                ..
            } => post_handle_recvs_conn(icid, moq_session, wt_conn, h3_conn, &mut conn.conn, appdata),
        }
    }

    // Phase 2: Forward pending subscriptions to publishers
    let to_forward = {
        let mut keys = Vec::new();
        for (nt, sub) in r.endpoint.app_data().subscriptions.iter() {
            if !sub.sent {
                keys.push(nt.clone());
            }
        }
        keys
    };
    let namespaces = r.endpoint.app_data().namespaces.clone();
    for nt in to_forward {
        if let Some(publisher) = namespaces.get(nt.namespace()) {
            if let Some(conn) = r.endpoint.conn_mut(*publisher) {
                if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
                    match moq.subscribe(&nt) {
                        Ok(request_id) => {
                            let app_data = r.endpoint.app_data_mut();
                            if let Some(sub) = app_data.subscriptions.get_mut(&nt) {
                                sub.sent = true;
                                sub.relay_request_id = Some(request_id);
                            }
                            app_data.relay_requests.insert(request_id, nt.clone());
                            info!("sent subscription {} to {}", nt, publisher);
                        }
                        Err(e) => {
                            error!("failed to subscribe {} on publisher {}: {:?}", nt, publisher, e);
                        }
                    }
                }
            }
        }
    }

    // Phase 3: Poll subscribe responses from publishers
    let pending_responses: Vec<(NamespaceTrackname, ClientId, RequestId)> = {
        let app_data = r.endpoint.app_data();
        app_data.subscriptions.iter()
            .filter(|(_, sub)| sub.relay_request_id.is_some() && sub.publisher_track_alias.is_none())
            .filter_map(|(nt, sub)| {
                sub.publisher_id.map(|pid| (nt.clone(), pid, sub.relay_request_id.unwrap()))
            })
            .collect()
    };

    for (nt, publisher_id, relay_req_id) in pending_responses {
        if let Some(conn) = r.endpoint.conn_mut(publisher_id) {
            if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
                if let Some(response) = moq.poll_subscribe_response(relay_req_id) {
                    match response {
                        Ok((track_alias, _)) => {
                            let sub = r.endpoint.app_data_mut().subscriptions.get_mut(&nt).unwrap();
                            sub.publisher_track_alias = Some(track_alias);
                            info!("publisher accepted {}", nt);
                        }
                        Err(e) => {
                            error!("publisher rejected {} with {} - {}", nt, e.error_code(), e.error_reason());
                            r.endpoint.app_data_mut().subscriptions.remove(&nt);
                            r.endpoint.app_data_mut().relay_requests.remove(&relay_req_id);
                        }
                    }
                }
            }
        }
    }

    // Phase 4: Forward object data from publishers to subscribers
    let active_tracks: Vec<(NamespaceTrackname, ClientId, TrackAlias, Vec<(ClientId, TrackAlias)>)> = {
        let app_data = r.endpoint.app_data();
        app_data.subscriptions.iter()
            .filter_map(|(nt, sub)| {
                let pub_ta = sub.publisher_track_alias?;
                let pub_id = sub.publisher_id?;
                let subs: Vec<_> = sub.subscribers.iter()
                    .filter_map(|s| s.track_alias.map(|ta| (s.client_id, ta)))
                    .collect();
                if subs.is_empty() { None }
                else { Some((nt.clone(), pub_id, pub_ta, subs)) }
            })
            .collect()
    };

    for (nt, publisher_id, pub_track_alias, subscribers) in active_tracks {
        // Take buffer state out of AppData to avoid borrow conflict with conn_mut
        let (mut obj_buf, mut obj_payload_len) = {
            let sub = r.endpoint.app_data_mut().subscriptions.get_mut(&nt).unwrap();
            (std::mem::take(&mut sub.obj_buf), sub.obj_payload_len)
        };

        let mut complete_objects: Vec<Vec<u8>> = Vec::new();

        // Read from publisher
        if let Some(conn) = r.endpoint.conn_mut(publisher_id) {
            if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
                loop {
                    // Read header if needed
                    if obj_payload_len == 0 {
                        match moq.read_obj_hdr(pub_track_alias) {
                            Ok(hdr) => {
                                obj_payload_len = hdr.payload_len();
                                obj_buf.clear();
                            }
                            Err(moq::Error::Done) => break,
                            Err(e) => {
                                error!("read obj hdr for {}: {:?}", nt, e);
                                break;
                            }
                        }
                    }

                    // Read payload
                    while obj_buf.len() < obj_payload_len {
                        let remaining = obj_payload_len - obj_buf.len();
                        let mut chunk = vec![0u8; remaining];
                        match moq.read_obj_pld(&mut chunk, pub_track_alias) {
                            Ok(n) => obj_buf.extend_from_slice(&chunk[..n]),
                            Err(moq::Error::Done) => break,
                            Err(e) => {
                                error!("read obj pld for {}: {:?}", nt, e);
                                break;
                            }
                        }
                    }

                    if obj_buf.len() >= obj_payload_len {
                        complete_objects.push(std::mem::take(&mut obj_buf));
                        obj_payload_len = 0;
                        // Try reading next object in this iteration
                    } else {
                        break; // Incomplete, try again next event loop iteration
                    }
                }
            }
        }

        // Put buffer state back
        {
            let sub = r.endpoint.app_data_mut().subscriptions.get_mut(&nt).unwrap();
            sub.obj_buf = obj_buf;
            sub.obj_payload_len = obj_payload_len;
        }

        // Forward complete objects to subscribers
        for obj_data in &complete_objects {
            for &(sub_id, sub_track_alias) in &subscribers {
                if let Some(conn) = r.endpoint.conn_mut(sub_id) {
                    if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
                        if let Err(e) = moq.send_obj(obj_data, sub_track_alias) {
                            error!("send obj to subscriber {} for {}: {:?}", sub_id, nt, e);
                        }
                    }
                }
            }
        }
    }
}

fn post_handle_recvs_conn(
    cid: ClientId,
    moq_session: &mut moq::MoqTransportSession,
    wt_conn: &mut wt::Connection,
    _h3_conn: &mut h3::Connection,
    quic_conn: &mut quiche::Connection,
    app_data: &mut AppData
) {
    loop {
        let Some((request_id, subscription)) = moq_session.subscription_inbox_next() else {
            break;
        };
        // Copy owned data to release the immutable borrow on moq_session
        let request_id = *request_id;
        let nt = subscription.namespace_trackname.clone();

        if app_data.namespaces.contains_key(nt.namespace()) {
            // Publisher known — accept immediately, forward data when available
            let track_alias = moq_session.accept_subscription(quic_conn, wt_conn, request_id);
            let publisher_id = *app_data.namespaces.get(nt.namespace()).unwrap();
            let sub = app_data.subscriptions.entry(nt.clone()).or_insert_with(|| {
                info!("new subscription {} from {} (publisher: {})", nt, cid, publisher_id);
                Subscription {
                    sent: false,
                    relay_request_id: None,
                    publisher_track_alias: None,
                    publisher_id: Some(publisher_id),
                    subscribers: Vec::new(),
                    obj_payload_len: 0,
                    obj_buf: Vec::new(),
                }
            });
            info!("accept subscriber {} for {}", cid, nt);
            sub.subscribers.push(SubscriberInfo {
                client_id: cid,
                request_id,
                track_alias: Some(track_alias),
            });
        } else {
            info!("reject subscription {} from {} (no publisher)", nt, cid);
            moq_session.reject_subscription(quic_conn, wt_conn, request_id, REQUEST_ERROR_DOES_NOT_EXIST);
        }
    }
    loop {
        let Some((&request_id, cm)) = moq_session.next_pending_namespace_publish() else {
            break;
        };
        let namespace = cm.track_namespace().clone();
        info!("accept publish namespace: {}", namespace);
        app_data.namespaces.insert(namespace, cid);
        moq_session.accept_namespace_publish(request_id, quic_conn, wt_conn);
    }
}

extern crate core;

use moq::OutgoingObject;
use chrono::Local;
use log::{info, trace};
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{quiche, EndpointConfig, ServerConfig};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use quiche_moq as moq;
use std::time::{Duration, Instant};
use boring::ssl::{SslContextBuilder, SslMethod};
use quiche_moq_webtransport_helper::{MoqWebTransportHelper, State};
use quiche_utils::cert::load_or_generate_keys;

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
}

impl Default for ConnAppData {
    fn default() -> Self {
        Self {
            moq_helper: MoqWebTransportHelper::new_server(moq::Config::default()),
        }
    }
}

struct AppData {
    /// instant when to send the next time object
    next_object_instant: Instant,
    /// group_id of the next time object — each is sent as its own one-object
    /// subgroup (opened and fin'd immediately), so this just needs to keep
    /// increasing across calls.
    next_group_id: u64,
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            next_object_instant: Instant::now() + Duration::from_secs(1),
            next_group_id: 0,
        }
    }
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

#[allow(clippy::field_reassign_with_default)]
fn main() {
    env_logger::builder().format_timestamp_nanos().init();
    let endpoint = Endpoint::new(
        Some({
            let mut c = ServerConfig::default();
            c.client_config = {
                let mut c = quiche::Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
                    let (cert, key) = load_or_generate_keys(&None, &None);
                    let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                    b.set_private_key(&key).unwrap();
                    b.set_certificate(&cert).unwrap();
                    b
                }).unwrap();
                MoqWebTransportHelper::configure_quic(&mut c);
                c
            };
            c
        }),
        EndpointConfig::default(),
        AppData::default(),
    );

    let socket = Socket::bind("0.0.0.0:8080").unwrap();
    info!("start server on {}", socket.local_addr);

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        endpoint,
    );

    runner.register_socket(socket);

    runner.run();
}

fn post_handle_recvs(runner: &mut Runner) {
    let now = Instant::now();
    let ad = runner.endpoint.app_data_mut();
    // date_time_str is None when no object should be sent now
    let (date_time_str, group_id) = if ad.next_object_instant <= now {
        ad.next_object_instant += Duration::from_secs(1);
        let group_id = ad.next_group_id;
        ad.next_group_id += 1;
        let str = Local::now().to_rfc3339();
        trace!("new date: {}", str);
        (Some(str), group_id)
    } else {
        (None, 0)
    };
    // timeout until the next time object
    let app_timeout = ad.next_object_instant.duration_since(now);
    runner.set_app_timeout(app_timeout);
    'conn: for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else { continue };
        let quic_conn = &mut conn.conn;
        conn.app_data.moq_helper.on_post_handle_recvs(quic_conn);
        let State::Moq {
            h3_conn,
            wt_conn,
            moq_session,
        } = &mut conn.app_data.moq_helper.state else {
            continue 'conn;
        };
        let (moq_session, mut t) = moq_session.core(wt_conn, h3_conn, quic_conn);

        // other requests are dropped, which rejects them
        while let Some(event) = moq_session.next_event() {
            if let moq::Event::Request(moq::IncomingRequest::Subscribe(request)) = event {
                let _ = moq_session.accept_request(request, moq::SubscribeOk::default(), &mut t);
            }
        }
        if let Some(date_time_str) = &date_time_str {
            const SUBGROUP_ID: u64 = 0;
            const OBJECT_ID: u64 = 0;
            let location = moq::wire::Location { group: group_id, object: OBJECT_ID };
            // if track is not writable skip the time object
            for track_alias in moq_session.writable() {
                let bytes = date_time_str.as_bytes();
                match moq_session.send_obj_hdr(track_alias, OutgoingObject { end_of_group: true, ..OutgoingObject::new(location, SUBGROUP_ID, bytes.len()) }, &mut t) {
                    Ok(_) => {}
                    Err(e) => unimplemented!("{:?}", e),
                }
                match moq_session.send_obj_pld(track_alias, location, bytes, &mut t) {
                    Ok(n) if n == bytes.len() => {}
                    other => unimplemented!("{:?}", other),
                }
                moq_session.fin_send_subgroup(track_alias, group_id, SUBGROUP_ID, &mut t);
            }
        }
        #[allow(clippy::never_loop)]
        for _ in moq_session.readable() {
            unimplemented!()
        }
    }
}

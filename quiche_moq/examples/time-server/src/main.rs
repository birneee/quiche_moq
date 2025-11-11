extern crate core;

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
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            next_object_instant: Instant::now() + Duration::from_secs(1),
        }
    }
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

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
        {
            let c = EndpointConfig::default();
            c
        },
        AppData::default(),
    );

    let socket = Socket::bind("0.0.0.0:8080".parse().unwrap(), false, false, false).unwrap();
    info!("start server on {}", socket.local_addr);

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        endpoint,
        None,
    );

    runner.register_socket(socket);

    runner.run();
}

fn post_handle_recvs(runner: &mut Runner) {
    let now = Instant::now();
    let next_object_instant = &mut runner.endpoint.app_data_mut().next_object_instant;
    // date_time_str in None when no object should be sent now
    let date_time_str = if *next_object_instant <= now {
        *next_object_instant = *next_object_instant + Duration::from_secs(1);
        let str = Local::now().to_rfc3339();
        trace!("new date: {}", str);
        Some(str)
    } else {
        None
    };
    // timeout until the next time object
    let app_timeout = next_object_instant.duration_since(now);
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

        while let Some(request_id) = moq_session.next_pending_received_subscription() {
            moq_session.accept_subscription(quic_conn, wt_conn, request_id);
        }
        if let Some(date_time_str) = &date_time_str {
            // if track is not writable skip the time object
            for track_alias in moq_session.writable() {
                match moq_session.send_obj(
                    date_time_str.as_bytes(),
                    track_alias,
                    wt_conn,
                    h3_conn,
                    quic_conn,
                ) {
                    Ok(_) => {}
                    Err(e) => unimplemented!("{:?}", e),
                }
            }
        }
        for _ in moq_session.readable() {
            unimplemented!()
        }
    }
}

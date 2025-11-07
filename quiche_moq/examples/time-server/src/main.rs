extern crate core;

use chrono::Local;
use log::{debug, info, trace};
use quiche_mio_runner::quiche_endpoint::quiche::{h3, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::{quiche, EndpointConfig, ServerConfig};
use quiche_h3_utils::{hdrs_to_strings, ALPN_HTTP_3};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use quiche_moq as moq;
use quiche_moq::{Config, MoqTransportSession};
use quiche_webtransport as wt;
use std::time::{Duration, Instant};
use boring::ssl::{SslContextBuilder, SslMethod};
use quiche_utils::cert::load_or_generate_keys;

struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    moq_session: Option<moq::MoqTransportSession>,
    wt_conn: quiche_webtransport::Connection,
}

impl Default for ConnAppData {
    fn default() -> Self {
        Self {
            h3_conn: None,
            moq_session: None,
            wt_conn: quiche_webtransport::Connection::new(true),
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
                c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
                c.set_initial_max_streams_bidi(100);
                c.set_initial_max_streams_uni(100);
                c.set_initial_max_data(10000000);
                c.set_initial_max_stream_data_bidi_remote(1000000);
                c.set_initial_max_stream_data_bidi_local(1000000);
                c.set_initial_max_stream_data_uni(1000000);
                c.enable_dgram(true, 100, 100);
                c.set_max_idle_timeout(30000);
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
        *next_object_instant = now + Duration::from_secs(1);
        let str = Local::now().to_rfc3339();
        trace!("new date: {}", str);
        Some(str)
    } else {
        None
    };
    // timout until the next time object
    let app_timeout = next_object_instant.duration_since(now);
    runner.set_app_timeout(app_timeout);
    for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else { continue };
        let quic_conn = &mut conn.conn;
        let h3_conn = match conn.app_data.h3_conn.as_mut() {
            Some(v) => v,
            None => {
                if !quic_conn.is_established() && !quic_conn.is_in_early_data() {
                    continue; // not ready for h3 yet
                }
                assert_eq!(quic_conn.application_proto(), ALPN_HTTP_3);
                conn.app_data.h3_conn = Some(h3::Connection::with_transport(
                    quic_conn,
                    &{
                        let mut c = h3::Config::new().unwrap();
                        wt::configure_h3(&mut c).unwrap();
                        c
                    },
                ).expect("Unable to create HTTP/3 connection, check the server's uni stream limit and window size"));
                conn.app_data.h3_conn.as_mut().unwrap()
            }
        };
        'h3_poll: loop {
            match h3_conn.poll(quic_conn) {
                Ok((stream_id, h3::Event::Headers { list, .. })) => {
                    debug!(
                        "h3 stream {} received headers: {:?}",
                        stream_id,
                        hdrs_to_strings(&list)
                    );
                    conn.app_data.wt_conn.recv_hdrs(stream_id, &list);
                }
                Ok(e) => unimplemented!("{:?}", e),
                Err(h3::Error::Done) => break 'h3_poll,
                Err(e) => unimplemented!("{:?}", e),
            }
        }
        let wt_conn = &mut conn.app_data.wt_conn;
        wt_conn.poll(h3_conn, quic_conn);

        let moq = match conn.app_data.moq_session.as_mut() {
            Some(moq) => moq,
            None => {
                let session_id = match wt_conn.readable_sessions().first() {
                    None => break,
                    Some(v) => *v,
                };
                conn.app_data.moq_session = Some(MoqTransportSession::accept(session_id.into(), Config::default()));
                conn.app_data.moq_session.as_mut().unwrap()
            }
        };
        moq.poll(quic_conn, h3_conn, wt_conn);
        while let Some(request_id) = moq.next_pending_received_subscription() {
            moq.accept_subscription(quic_conn, wt_conn, request_id);
        }
        if let Some(date_time_str) = &date_time_str {
            // if track is not writable skip the time object
            for track_alias in moq.writable() {
                match moq.send_obj(
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
        for _ in moq.readable() {
            unimplemented!()
        }
    }
}

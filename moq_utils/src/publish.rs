extern crate core;

use crate::args::PublishArgs;
use log::{debug, info};
use quiche_mio_runner::quiche_endpoint::quiche::{h3, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::{quiche, EndpointConfig};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use quiche_moq as moq;
use quiche_moq::MoqTransportSession;
use quiche_webtransport as wt;
use std::fs;
use url::Url;
use crate::h3::{hdrs_to_strings, ALPN_HTTP_3};

struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    moq_session: Option<MoqTransportSession>,
    wt_conn: quiche_webtransport::Connection,
    /// The WebTransport session id used for MoQ
    moq_session_id: Option<u64>,
    url: Url,
    moq_initialized_msg: bool,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, ()>;
type Runner = runner::Runner<ConnAppData, (), ()>;

pub(crate) fn run_publish(args: &PublishArgs) {
    let mut endpoint = Endpoint::new(
        None,
        {
            let c = EndpointConfig::default();
            c
        },
        (),
    );

    let socket = Socket::bind("[::]:0".parse().unwrap(), false, false, false).unwrap();

    let url = Url::parse(&args.url).unwrap();
    let peer_addr = url
        .socket_addrs(|| Some(443))
        .unwrap()
        .first()
        .unwrap()
        .clone();

    let keylog = args.ssl_key_log_file.as_ref()
        .map(|p| fs::OpenOptions::new().create(true).append(true).open(p).unwrap());

    info!("connect to {}", peer_addr);

    let icid = endpoint.connect(
        None,
        socket.local_addr,
        peer_addr,
        &mut {
            let mut c = quiche::Config::new(PROTOCOL_VERSION).unwrap();
            c.verify_peer(false);
            c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
            c.set_initial_max_streams_bidi(100);
            c.set_initial_max_streams_uni(100);
            c.set_initial_max_data(10000000);
            c.set_initial_max_stream_data_bidi_remote(1000000);
            c.set_initial_max_stream_data_bidi_local(1000000);
            c.set_initial_max_stream_data_uni(1000000);
            c.enable_dgram(true, 100, 100);
            c.set_max_idle_timeout(30000);
            if keylog.is_some() {
                c.log_keys()
            }
            c
        },
        ConnAppData {
            h3_conn: None,
            wt_conn: wt::Connection::new(),
            moq_session: None,
            moq_session_id: None,
            url,
            moq_initialized_msg: false,
        },
        None,
        None,
    );

    if let Some(keylog) = keylog {
        endpoint.conn_mut(icid).unwrap().conn.set_keylog(Box::new(keylog));
    }

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
    for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else { continue };
        let quic_conn = &mut conn.conn;
        let h3_conn = match conn.app_data.h3_conn.as_mut() {
            Some(v) => v,
            None => {
                if !quic_conn.is_established() && !quic_conn.is_in_early_data() {
                    continue; // not ready for h3 yet
                }
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
        loop {
            match h3_conn.poll(quic_conn) {
                Ok((stream_id, h3::Event::Headers { list, .. })) => {
                    debug!(
                        "h3 stream {} received headers: {:?}",
                        stream_id,
                        hdrs_to_strings(&list)
                    );
                    if Some(stream_id) == conn.app_data.moq_session_id {
                        conn.app_data.wt_conn.recv_hdrs(stream_id, &list);
                    }
                }
                Ok(e) => unimplemented!("{:?}", e),
                Err(h3::Error::Done) => break,
                Err(e) => unimplemented!("{:?}", e),
            }
        }
        let wt_conn = &mut conn.app_data.wt_conn;
        wt_conn.poll(h3_conn, quic_conn);
        let moq_session_id = match conn.app_data.moq_session_id {
            None => {
                if !wt::webtransport_enabled_by_server(&h3_conn) {
                    continue; // not ready for wt
                }
                conn.app_data.moq_session_id = Some(wt_conn.connect_session(h3_conn, quic_conn, conn.app_data.url.clone()));
                conn.app_data.moq_session_id.unwrap()
            }
            Some(v) => v,
        };
        let moq_session = match conn.app_data.moq_session.as_mut() {
            Some(v) => v,
            None => {
                if !wt_conn.established(moq_session_id) {
                    continue; // not ready for moq
                }
                conn.app_data.moq_session = Some(MoqTransportSession::connect(
                    moq_session_id.into(),
                    h3_conn,
                    quic_conn,
                    wt_conn,
                    {
                        let mut c = moq::Config::default();
                        c.setup_version = moq::wire::MOQ_VERSION_DRAFT_07;
                        c.ignore_max_request_quota = true;
                        c
                    },
                ));
                conn.app_data.moq_session.as_mut().unwrap()
            }
        };
        moq_session.poll(quic_conn, h3_conn, wt_conn);
        if moq_session.initialized() && !conn.app_data.moq_initialized_msg{
            conn.app_data.moq_initialized_msg = true;
            info!("moq initialized");
        }
    }
    //todo collect garbage
}

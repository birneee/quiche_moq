extern crate core;

use log::{debug, info};
use quiche_mio_runner::quiche_endpoint::quiche::{h3, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::{quiche, EndpointConfig};
use quiche_h3_utils::{hdrs_to_strings, ALPN_HTTP_3};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use quiche_moq as moq;
use quiche_moq::MoqTransportSession;
use quiche_webtransport as wt;

struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    moq_session: Option<MoqTransportSession>,
    wt_conn: quiche_webtransport::Connection,
    subscribed: bool,
    /// The WebTransport session id used for MoQ
    moq_session_id: Option<u64>,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, ()>;
type Runner = runner::Runner<ConnAppData, (), ()>;

fn main() {
    env_logger::builder().format_timestamp_nanos().init();
    let mut endpoint = Endpoint::new(
        None,
        {
            let c = EndpointConfig::default();
            c
        },
        (),
    );

    let socket = Socket::bind("0.0.0.0:0".parse().unwrap(), false, false, false).unwrap();

    let server_addr = "127.0.0.1:8080".parse().unwrap();

    info!("connect to {}", server_addr);

    endpoint.connect(
        None,
        socket.local_addr,
        server_addr,
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
            c
        },
        ConnAppData {
            h3_conn: None,
            wt_conn: wt::Connection::new(),
            moq_session: None,
            subscribed: false,
            moq_session_id: None,
        },
        None,
        None,
    );

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
                conn.app_data.moq_session_id = Some(wt_conn.connect_session(h3_conn, quic_conn, "https://example.org".parse().unwrap()));
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
                    Default::default(),
                ));
                conn.app_data.moq_session.as_mut().unwrap()
            }
        };
        moq_session.poll(quic_conn, h3_conn, wt_conn);
        if moq_session.initialized() {
            if !conn.app_data.subscribed {
                info!("subscribe clock second");
                moq_session.subscribe(
                    quic_conn,
                    wt_conn,
                    vec![Vec::from(b"clock")],
                    Vec::from(b"second"),
                ).unwrap();
                conn.app_data.subscribed = true;
            }
        }
        'trackLoop: for track_alias in &moq_session.readable() {
            loop {
                let hdr = match moq_session.read_obj_hdr(*track_alias, wt_conn, h3_conn, quic_conn)
                {
                    Ok(v) => v,
                    Err(moq::Error::Done) => continue 'trackLoop,
                    Err(e) => unimplemented!("{:?}", e),
                };
                let mut buf = [0u8; 100];
                let n = match moq_session.read_obj_pld(
                    &mut buf,
                    *track_alias,
                    wt_conn,
                    h3_conn,
                    quic_conn,
                ) {
                    Ok(v) => v,
                    Err(e) => unimplemented!("{:?}", e),
                };
                assert_eq!(n, hdr.payload_len());
                let pld = &buf[..n];
                debug!("moq obj {:?} {:?}", hdr, pld);
                info!("received {:?}", String::from_utf8_lossy(&pld));
            }
        }
    }
    //todo collect garbage
}

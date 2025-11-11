extern crate core;

use log::info;
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{quiche, EndpointConfig};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use std::io;
use quiche_moq as moq;
use std::io::{Stdout, Write};
use quiche_moq_webtransport_helper::{MoqWebTransportHelper, State};

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
    subscribed: bool,
}

struct AppData {
    out: Stdout,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

fn main() {
    let out = io::stdout();

    env_logger::builder().format_timestamp_nanos().init();
    let mut endpoint = Endpoint::new(
        None,
        {
            let c = EndpointConfig::default();
            c
        },
        AppData { out },
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
            MoqWebTransportHelper::configure_quic(&mut c);
            c.verify_peer(false);
            c
        },
        ConnAppData {
            moq_helper: MoqWebTransportHelper::new_client("https://example.org".parse().unwrap(), moq::Config::default()),
            subscribed: false,
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
    'conn: for icid in &mut runner.endpoint.conn_index_iter() {
        let (conn, app_data) = runner.endpoint.conn_with_app_data_mut(icid);
        let Some(conn) = conn else { continue };
        let quic_conn = &mut conn.conn;
        conn.app_data.moq_helper.on_post_handle_recvs(quic_conn);
        let State::Moq {
            h3_conn,
            wt_conn,
            moq_session,
        } = &mut conn.app_data.moq_helper.state else {
            continue 'conn;
        };
        if moq_session.initialized() {
            if !conn.app_data.subscribed {
                info!("subscribe clock second");
                moq_session.subscribe(
                    quic_conn,
                    wt_conn,
                    vec![Vec::from(b"testsrc")],
                    Vec::from(b"mp4"),
                ).unwrap();
                conn.app_data.subscribed = true;
            }
        }
        'trackLoop: for track_alias in moq_session.readable() {
            loop {
                let rop = moq_session.remaining_object_payload(track_alias).unwrap();
                if rop == 0 {
                    match moq_session.read_obj_hdr(track_alias, wt_conn, h3_conn, quic_conn) {
                        Ok(_) => {}
                        Err(quiche_moq::Error::Fin) => continue 'trackLoop,
                        Err(quiche_moq::Error::Done) => continue 'trackLoop,
                        Err(e) => unimplemented!("{:?}", e),
                    };
                }
                let mut buf = [0u8; 1000];
                let n = match moq_session.read_obj_pld(
                    &mut buf,
                    track_alias,
                    wt_conn,
                    h3_conn,
                    quic_conn,
                ) {
                    Ok(n) => n,
                    Err(e) => unimplemented!("{:?}", e),
                };
                app_data.out.write_all(&buf[..n]).unwrap();
            }
        }
    }
    //todo collect garbage
}

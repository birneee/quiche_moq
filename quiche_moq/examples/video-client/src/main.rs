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
    subscription: Option<quiche_moq::Subscription>,
    /// The object currently being read (from `recv_new_obj_hdr`) and how
    /// many payload bytes are still owed for it — `recv_obj_pld` needs the
    /// `Location` on every call now that a track can have several objects
    /// in flight at once, so this example (which only ever reads one at a
    /// time) tracks it itself.
    current: Option<(moq::wire::Location, usize)>,
}

struct AppData {
    out: Stdout,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

#[allow(clippy::field_reassign_with_default)]
fn main() {
    let out = io::stdout();

    env_logger::builder().format_timestamp_nanos().init();
    let mut endpoint = Endpoint::new(
        None,
        EndpointConfig::default(),
        AppData { out },
    );

    let socket = Socket::bind("0.0.0.0:0").unwrap();

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
            subscription: None,
            current: None,
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
        let (moq_session, mut t) = moq_session.core(wt_conn, h3_conn, quic_conn);
        if moq_session.initialized() && conn.app_data.subscription.is_none() {
            info!("subscribe clock second");
            conn.app_data.subscription = Some(moq_session.subscribe(
                &"testsrc--mp4".parse().unwrap(),
                quiche_moq::SubscribeOptions { filter: quiche_moq::LocationFilter::NextGroupStart, ..Default::default() },
                &mut t).unwrap());
        }
        'trackLoop: for track_alias in moq_session.readable() {
            loop {
                let (location, remaining) = match conn.app_data.current {
                    Some(v) => v,
                    None => match moq_session.recv_new_obj_hdr(track_alias, &mut t) {
                        Ok(quiche_moq::RecvObjHdr::Header { location, header, .. }) => (location, header.payload_len()),
                        Ok(quiche_moq::RecvObjHdr::SubgroupFinished { .. } | quiche_moq::RecvObjHdr::SubgroupReset { .. } | quiche_moq::RecvObjHdr::Datagram { .. }) => continue,
                        Err(quiche_moq::Error::Fin) => continue 'trackLoop,
                        Err(quiche_moq::Error::Done) => continue 'trackLoop,
                        Err(e) => unimplemented!("{:?}", e),
                    },
                };
                let mut buf = [0u8; 1000];
                let want = remaining.min(buf.len());
                let n = match moq_session.recv_obj_pld(
                    track_alias,
                    location,
                    &mut buf[..want], &mut t) {
                    Ok(n) => n,
                    Err(e) => unimplemented!("{:?}", e),
                };
                conn.app_data.current = if remaining - n > 0 { Some((location, remaining - n)) } else { None };
                app_data.out.write_all(&buf[..n]).unwrap();
            }
        }
    }
    //todo collect garbage
}

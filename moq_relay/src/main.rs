use boring::ssl::{SslContextBuilder, SslMethod};
use log::{LevelFilter, error, info};
use quiche_mio_runner as runner;
use quiche_mio_runner::Socket;
use quiche_mio_runner::quiche_endpoint::quiche::h3;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig, quiche};
use quiche_moq as moq;
use quiche_moq_webtransport_helper::{MoqWebTransportHelper, State};
use quiche_utils::cert::load_or_generate_keys;
use quiche_webtransport as wt;

type Runner = runner::Runner<AppData, (), ()>;

struct AppData {
    moq_helper: MoqWebTransportHelper,
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
            let mut c = runner::Config::<AppData, (), ()>::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        Endpoint::new(
            Some({
                let mut c = ServerConfig::new(|_| AppData {
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
            (),
        ),
        None,
    );
    r.register_socket(socket);
    r.run();
}

fn post_handle_recvs(r: &mut Runner) {
    for icid in &mut r.endpoint.conn_index_iter() {
        let Some(conn) = r.endpoint.conn_mut(icid) else {
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
            } => post_handle_recvs_conn(moq_session, wt_conn, h3_conn, &mut conn.conn),
        }
    }
}

fn post_handle_recvs_conn(
    moq_session: &mut moq::MoqTransportSession,
    wt_conn: &mut wt::Connection,
    h3_conn: &mut h3::Connection,
    quic_conn: &mut quiche::Connection,
) {
    while let Some(request_id) = moq_session.next_pending_received_subscription() {
        let subscription = moq_session.pending_received_subscription(request_id);
        if subscription.namespace_trackname != "meeting--video".parse().unwrap() {
            unreachable!()
        }
        info!("accept track {}", subscription.namespace_trackname);
        let track_alias = moq_session.accept_subscription(quic_conn, wt_conn, request_id);
        let buf = b"hello";
        moq_session
            .send_obj(buf, track_alias, wt_conn, h3_conn, quic_conn)
            .unwrap();
        info!("send obj: {}", str::from_utf8(buf).unwrap())
    }
    while let Some((&request_id, cm)) = moq_session.next_pending_namespace_publish() {
        info!("accept publish namespace: {}", cm.track_namespace());
        moq_session.accept_namespace_publish(request_id, quic_conn, wt_conn);
    }
}

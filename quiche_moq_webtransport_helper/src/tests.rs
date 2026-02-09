use std::io::Write;
use std::thread;
use boring::ssl::{SslContextBuilder, SslMethod};
use boring::x509::store::X509StoreBuilder;
use log::{info, LevelFilter};
use quiche::PROTOCOL_VERSION;
use runner::Runner;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig};
use quiche_mio_runner::{mio, Socket};
use crate::{MoqWebTransportHelper, State};
use quiche_moq as moq;
use quiche_moq::wire::{RequestId, TrackAlias};

/// for client and server
fn quic_config() -> (quiche::Config, quiche::Config) {
    let (key, cert) = quiche_mio_runner::quiche_endpoint::test_utils::key_pair();
    (
        {
            let mut c = quiche::Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
                let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                b.set_cert_store_builder({
                    let mut b = X509StoreBuilder::new().unwrap();
                    b.add_cert(cert.clone()).unwrap();
                    b
                });
                b
            }).unwrap();
            MoqWebTransportHelper::configure_quic(&mut c);
            c.set_max_idle_timeout(1000);
            c
        },
        {
            let mut c = quiche::Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
                let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                b.set_private_key(&key).unwrap();
                b.set_certificate(&cert).unwrap();
                b
            }).unwrap();
            MoqWebTransportHelper::configure_quic(&mut c);
            c.set_max_idle_timeout(1000);
            c
        }
    )
}

#[test]
fn handshake() {
    let _ = env_logger::builder().filter(None, LevelFilter::Info).format_timestamp_nanos().try_init();
    let server_socket = Socket::bind("127.0.0.0:0").unwrap();
    let server_addr = server_socket.local_addr;
    let (mut client_quic_config, server_quic_config) = quic_config();
    // run client
    let cj = thread::spawn(move || {
        struct AppData {
            moq_helper: MoqWebTransportHelper,
            subscribed: bool,
            request_id: Option<RequestId>,
            track_alias: Option<TrackAlias>,
            recv_obj_count: usize,
        }
        let socket = Socket::bind("127.0.0.0:0").unwrap();
        let mut r = Runner::new(
            {
                let mut c = runner::Config::<AppData, (), ()>::default();
                c.post_handle_recvs = |r| {
                    'conn: for icid in &mut r.endpoint.conn_index_iter() {
                        let Some(conn) = r.endpoint.conn_mut(icid) else { continue };
                        let quic_conn = &mut conn.conn;
                        conn.app_data.moq_helper.on_post_handle_recvs(quic_conn);
                        let State::Moq {
                            moq_session,
                            h3_conn,
                            wt_conn,
                            ..
                        } = &mut conn.app_data.moq_helper.state else {
                            continue 'conn;
                        };
                        if !conn.app_data.subscribed {
                            let name = "meeting--video".parse().unwrap();
                            conn.app_data.request_id = Some(moq_session.subscribe(&mut conn.conn, wt_conn, &name).unwrap());
                            info!("subscribe {}", name);
                            conn.app_data.subscribed = true;
                        }
                        if let Some(request_id) = conn.app_data.request_id && let Some(resp) = moq_session.poll_subscribe_response(request_id) {
                            conn.app_data.track_alias = Some(resp.unwrap().0)
                        }
                        if let Some(track_alias) = conn.app_data.track_alias {
                            let hdr = match moq_session.read_obj_hdr(track_alias, wt_conn, h3_conn,  &mut conn.conn) {
                                Ok(v) => v,
                                Err(quiche_moq::Error::Done) => continue,
                                Err(e) => unimplemented!("{:?}", e),
                            };
                            let mut buf = [0u8; 100];
                            let n = moq_session.read_obj_pld(&mut buf, track_alias, wt_conn, h3_conn,  &mut conn.conn).unwrap();
                            assert_eq!(n, hdr.payload_len());
                            info!("recv obj: {}", str::from_utf8(&buf[..n]).unwrap());
                            conn.app_data.recv_obj_count += 1;
                            r.close()
                        }
                    }
                };
                c
            },
            {
                let mut e = Endpoint::new(None, EndpointConfig::default(), ());
                e.connect(
                    None,
                    socket.local_addr,
                    server_addr,
                    &mut client_quic_config,
                    AppData {
                        moq_helper: MoqWebTransportHelper::new_client("https://example.org".parse().unwrap(), moq::Config::default()),
                        subscribed: false,
                        request_id: None,
                        track_alias: None,
                        recv_obj_count: 0,
                    },
                    None,
                    None,
                );
                e
            },
            None,
        );
        r.register_socket(socket);
        r.run();
        r.endpoint.conn(0).unwrap().app_data.recv_obj_count
    });
    // run server
    let (mut close_pipe_tx, mut close_pipe_rx) = mio::unix::pipe::new().unwrap();
    let sj = thread::spawn(move || {
        struct AppData {
            moq_helper: MoqWebTransportHelper,
        }
        let mut r = Runner::new(
            {
                let mut c = runner::Config::<AppData, (), ()>::default();
                c.post_handle_recvs = |r| {
                    'conn: for icid in &mut r.endpoint.conn_index_iter() {
                        let Some(conn) = r.endpoint.conn_mut(icid) else { continue };
                        let quic_conn = &mut conn.conn;
                        conn.app_data.moq_helper.on_post_handle_recvs(quic_conn);
                        let State::Moq {
                            wt_conn,
                            h3_conn,
                            moq_session, ..
                        } = &mut conn.app_data.moq_helper.state else {
                            continue 'conn;
                        };

                        while let Some(request_id) = moq_session.next_pending_received_subscription() {
                            let subscription = moq_session.pending_received_subscription(request_id);
                            if subscription.namespace_trackname != "meeting--video".parse().unwrap() {
                                unreachable!()
                            }
                            info!("accept track {}", subscription.namespace_trackname);
                            let track_alias = moq_session.accept_subscription(&mut conn.conn, wt_conn, request_id);
                            let buf = b"hello";
                            moq_session.send_obj(buf, track_alias, wt_conn, h3_conn, &mut conn.conn).unwrap();
                            info!("send obj: {}", str::from_utf8(buf).unwrap())
                        }
                    }
                };
                c
            },
            {
                let e = Endpoint::new(Some({
                    let mut c = ServerConfig::new(|_| {
                        AppData {
                            moq_helper: MoqWebTransportHelper::new_server(moq::Config::default()),
                        }
                    });
                    c.client_config = server_quic_config;
                    c
                }), EndpointConfig::default(), ());
                e
            },
            Some(&mut close_pipe_rx),
        );
        r.register_socket(server_socket);
        r.run();
    });
    let recv_obj_count = cj.join().unwrap();
    assert_eq!(recv_obj_count, 1);
    close_pipe_tx.write(&[0]).unwrap();
    sj.join().unwrap();
}

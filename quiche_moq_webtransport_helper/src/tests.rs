use quiche_moq::OutgoingObject;
use std::io::Write;
use quiche_moq::SubscribeOptions;
use std::thread;
use boring::ssl::{SslContextBuilder, SslMethod};
use boring::x509::store::X509StoreBuilder;
use log::{info, LevelFilter};
use quiche::PROTOCOL_VERSION;
use runner::Runner;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, ServerConfig};
use quiche_mio_runner::{mio, Socket};
use crate::MoqWebTransportHelper;
use quiche_moq as moq;
use quiche_moq::wire::{Location, TrackAlias};
use quiche_moq::{Event, IncomingRequest, SubscribeOk};

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
                b.set_private_key(key).unwrap();
                b.set_certificate(cert).unwrap();
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
            subscription: Option<moq::Subscription>,
            track_alias: Option<TrackAlias>,
            recv_obj_count: usize,
        }
        let socket = Socket::bind("127.0.0.0:0").unwrap();
        let mut r = Runner::new(
            {
                runner::Config::<AppData, (), ()> {
                    post_handle_recvs: |r| {
                    'conn: for icid in &mut r.endpoint.conn_index_iter() {
                        let Some(conn) = r.endpoint.conn_mut(icid) else { continue };
                        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
                        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else {
                            continue 'conn;
                        };
                        if conn.app_data.subscription.is_none() {
                            let name = "meeting--video".parse().unwrap();
                            conn.app_data.subscription = Some(moq.session.subscribe(&name, SubscribeOptions { filter: quiche_moq::LocationFilter::NextGroupStart, ..Default::default() }, &mut moq.t).unwrap());
                            info!("subscribe {}", name);
                        }
                        while let Some(event) = moq.session.next_event() {
                            if let quiche_moq::Event::SubscribeOk { request_id, track_alias, .. } = event && conn.app_data.subscription.as_ref().is_some_and(|s| s.request_id() == request_id) {
                                conn.app_data.track_alias = Some(track_alias);
                            }
                        }
                        if let Some(track_alias) = conn.app_data.track_alias {
                            let (location, hdr) = match moq.session.recv_new_obj_hdr(track_alias, &mut moq.t) {
                                Ok(quiche_moq::RecvObjHdr::Header { location, header, .. }) => (location, header),
                                Ok(quiche_moq::RecvObjHdr::SubgroupFinished { .. } | quiche_moq::RecvObjHdr::SubgroupReset { .. } | quiche_moq::RecvObjHdr::Datagram { .. }) => {
                                    continue;
                                },
                                Err(quiche_moq::Error::Done) => {
                                    continue;
                                },
                                Err(e) => unimplemented!("{:?}", e),
                            };
                            let mut buf = [0u8; 100];
                            let n = moq.session.recv_obj_pld(track_alias, location, &mut buf, &mut moq.t).unwrap();
                            assert_eq!(n, hdr.payload_len());
                            info!("recv obj: {}", str::from_utf8(&buf[..n]).unwrap());
                            conn.app_data.recv_obj_count += 1;
                            r.close()
                        }
                    }
                    },
                    ..Default::default()
                }
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
                        subscription: None,
                        track_alias: None,
                        recv_obj_count: 0,
                    },
                    None,
                    None,
                );
                e
            },
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
            publications: Vec<moq::Publication>,
        }
        let mut r = Runner::new(
            {
                runner::Config::<AppData, (), ()> {
                    post_handle_recvs: |r| {
                    'conn: for icid in &mut r.endpoint.conn_index_iter() {
                        let Some(conn) = r.endpoint.conn_mut(icid) else { continue };
                        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
                        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else {
                            continue 'conn;
                        };

                        while let Some(Event::Request(IncomingRequest::Subscribe(request))) = moq.session.next_event() {
                            let nt = request.message().namespace_trackname.clone();
                            assert_eq!(nt, "meeting--video".parse().unwrap());
                            info!("accept track {nt}");
                            let publication = moq.session.accept_request(request, SubscribeOk::default(), &mut moq.t).unwrap();
                            let track_alias = publication.track_alias();
                            conn.app_data.publications.push(publication);
                            let buf = b"hello";
                            moq.session.send_obj_hdr(track_alias, OutgoingObject::new(Location { group: 0, object: 0 }, 0, buf.len()), &mut moq.t).unwrap();
                            let n = moq.session.send_obj_pld(track_alias, Location { group: 0, object: 0 }, buf, &mut moq.t).unwrap();
                            assert_eq!(n, buf.len());
                            info!("send obj: {}", str::from_utf8(buf).unwrap());
                        }
                    }
                    },
                    ..Default::default()
                }
            },
            {
                
                Endpoint::new(Some({
                    let mut c = ServerConfig::new(|_| {
                        AppData {
                            moq_helper: MoqWebTransportHelper::new_server(moq::Config::default()),
                            publications: Vec::new(),
                        }
                    });
                    c.client_config = server_quic_config;
                    c
                }), EndpointConfig::default(), ())
            },
        );
        r.register_socket(server_socket);
        r.registry().register_close(&mut close_pipe_rx);
        r.run();
    });
    let recv_obj_count = cj.join().unwrap();
    assert_eq!(recv_obj_count, 1);
    close_pipe_tx.write_all(&[0]).unwrap();
    sj.join().unwrap();
}

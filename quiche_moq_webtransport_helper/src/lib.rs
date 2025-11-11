use quiche;
use quiche::h3;
use quiche_moq as moq;
use quiche_moq::MoqTransportSession;
use quiche_webtransport as wt;
use std::mem;
use log::debug;
use url::Url;
use quiche_h3_utils::ALPN_HTTP_3;

/// Make it easy to write an MoQ WebTransport clients and servers
pub struct MoqWebTransportHelper {
    pub state: State,
    moq_config: moq::Config,
    perspective: Perspective,
}

impl MoqWebTransportHelper {
    pub fn new_client(url: Url, moq_config: moq::Config) -> Self {
        Self {
            state: State::Quic,
            moq_config,
            perspective: Perspective::Client {
                url,
            }
        }
    }

    pub fn new_server(moq_config: moq::Config) -> Self {
        Self {
            state: State::Quic,
            moq_config,
            perspective: Perspective::Server {}
        }
    }

    /// this function must be called when new quic packets have been received or the timeout fired
    pub fn on_post_handle_recvs(&mut self, quic_conn: &mut quiche::Connection) {
        'conn: loop {
            match &mut self.state {
                State::Quic => {
                    if !quic_conn.is_established() && !quic_conn.is_in_early_data() {
                        break 'conn; // not ready for h3 yet
                    }
                    let h3_conn = h3::Connection::with_transport(
                        quic_conn,
                        &{
                            let mut c = h3::Config::new().unwrap();
                            wt::configure_h3(&mut c).unwrap();
                            c
                        },
                    ).expect("Unable to create HTTP/3 connection, check the server's uni stream limit and window size");
                    self.state = State::H3 {
                        h3_conn,
                        wt_conn: wt::Connection::new(matches!(self.perspective, Perspective::Server))
                    }
                }
                State::H3 { h3_conn, wt_conn } => {
                    match &mut self.perspective {
                        Perspective::Client { url } => {
                            Self::h3_poll_expect_nothing(h3_conn, quic_conn);
                            if !wt::webtransport_enabled_by_server(&h3_conn) {
                                break 'conn; // not ready for wt
                            }
                            let moq_session_id =
                                wt_conn.connect_session(h3_conn, quic_conn, url.clone());
                            let State::H3 { h3_conn, wt_conn } = mem::replace(&mut self.state, State::Quic) else {
                                unreachable!()
                            };
                            self.state = State::Wt {
                                h3_conn,
                                wt_conn,
                                moq_session_id,
                            };
                        }
                        Perspective::Server => {
                            'h3: loop {
                                match h3_conn.poll(quic_conn) {
                                    Ok((stream_id, h3::Event::Headers { list, .. })) => {
                                        debug!(
                                            "h3 stream {} received headers",
                                            stream_id,
                                        );
                                        wt_conn.recv_hdrs(stream_id, &list);
                                    }
                                    Ok(e) => unimplemented!("{:?}", e),
                                    Err(h3::Error::Done) => break 'h3,
                                    Err(e) => unimplemented!("{:?}", e),
                                }
                            }
                            let Some(&moq_session_id) = wt_conn.session_ids().first() else { break 'conn };
                            let moq_session = MoqTransportSession::accept(moq_session_id.into(), self.moq_config.clone());
                            let State::H3 { h3_conn, wt_conn } = mem::replace(&mut self.state, State::Quic) else {
                                unreachable!()
                            };
                            self.state = State::Moq {
                                h3_conn,
                                wt_conn,
                                moq_session,
                            };
                        }
                    }
                }
                State::Wt {
                    h3_conn,
                    wt_conn,
                    moq_session_id,
                } => {
                    assert!(matches!(self.perspective, Perspective::Client { ..  }));
                    'h3: loop {
                        match h3_conn.poll(quic_conn) {
                            Ok((stream_id, h3::Event::Headers { list, .. })) => {
                                wt_conn.recv_hdrs(stream_id, &list);
                            }
                            Ok(e) => unimplemented!("{:?}", e),
                            Err(h3::Error::Done) => break 'h3,
                            Err(e) => unimplemented!("{:?}", e),
                        }
                    }
                    wt_conn.poll(h3_conn, quic_conn);
                    if !wt_conn.established(*moq_session_id) {
                        break 'conn; // not ready for moq
                    }
                    let moq_session = MoqTransportSession::connect(
                        (*moq_session_id).into(),
                        h3_conn,
                        quic_conn,
                        wt_conn,
                        self.moq_config.clone(),
                    );
                    let State::Wt {
                        h3_conn, wt_conn, ..
                    } = mem::replace(&mut self.state, State::Quic)
                    else {
                        unreachable!()
                    };
                    self.state = State::Moq {
                        h3_conn,
                        wt_conn,
                        moq_session,
                    };
                }
                State::Moq {
                    h3_conn,
                    wt_conn,
                    moq_session,
                } => {
                    Self::h3_poll_expect_nothing(h3_conn, quic_conn);
                    wt_conn.poll(h3_conn, quic_conn);
                    moq_session.poll(quic_conn, h3_conn, wt_conn);
                    break 'conn;
                }
            }
        }
    }

    fn h3_poll_expect_nothing(h3_conn: &mut h3::Connection, quic_conn: &mut quiche::Connection) {
        'h3: loop {
            match h3_conn.poll(quic_conn) {
                Ok((_, h3::Event::Headers { .. })) => unreachable!("unexpected h3 response"),
                Ok(e) => unimplemented!("{:?}", e),
                Err(h3::Error::Done) => break 'h3,
                Err(e) => unimplemented!("{:?}", e),
            }
        }
    }

    pub fn configure_quic(c: &mut quiche::Config) {
        c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
        c.set_initial_max_streams_bidi(100);
        c.set_initial_max_streams_uni(100);
        c.set_initial_max_data(10_000_000);
        c.set_initial_max_stream_data_bidi_remote(1_000_000);
        c.set_initial_max_stream_data_bidi_local(1_000_000);
        c.set_initial_max_stream_data_uni(1_000_000);
        c.enable_dgram(true, 100, 100);
        c.set_max_idle_timeout(30000);
    }
}

pub enum State {
    Quic,
    H3 {
        h3_conn: h3::Connection,
        wt_conn: wt::Connection,
    },
    Wt {
        h3_conn: h3::Connection,
        wt_conn: wt::Connection,
        /// The WebTransport session id used for MoQ
        moq_session_id: u64,
    },
    Moq {
        h3_conn: h3::Connection,
        wt_conn: wt::Connection,
        moq_session: MoqTransportSession,
    },
}

enum Perspective {
    Client {
        url: Url
    },
    Server
}

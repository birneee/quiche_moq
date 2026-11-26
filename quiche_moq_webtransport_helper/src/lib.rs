#[cfg(test)]
mod tests;

pub use quiche_moq::MoqHandle;

use quiche::h3;
use quiche_moq as moq;
use quiche_moq::MoqTransportSession;
use quiche_webtransport as wt;
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
            perspective: Perspective::Client { url },
        }
    }

    pub fn new_server(moq_config: moq::Config) -> Self {
        Self {
            state: State::Quic,
            moq_config,
            perspective: Perspective::Server { native_quic: false },
        }
    }

    /// A server that also takes native QUIC sessions, offered by `set_application_protos`.
    /// - quiche names the ALPN only once the handshake completes, WebTransport starts then too
    ///   instead of sending SETTINGS in 0.5-RTT
    pub fn with_native_quic(mut self) -> Self {
        if let Perspective::Server { native_quic } = &mut self.perspective {
            *native_quic = true;
        }
        self
    }

    /// Mutable access to the configured MoQ-transport version(s) for this
    /// connection's SETUP handshake. The `MoqTransportSession` is constructed
    /// lazily, from a clone of this config, once the underlying QUIC/H3/
    /// WebTransport layers establish — so changes here only take effect if
    /// made before that point (i.e. while `is_ready()` is still `false`).
    pub fn moq_config_mut(&mut self) -> &mut moq::Config {
        &mut self.moq_config
    }

    /// Returns a handle to the MoQ session if ready, None otherwise.
    /// Returns the MoQ session if ready, None otherwise.
    pub fn moq_session(&mut self) -> Option<&mut MoqTransportSession> {
        match &mut self.state {
            State::Moq { moq_session, .. } => Some(moq_session),
            _ => None,
        }
    }

    /// This bundles all connection references for ergonomic API calls.
    /// Returns None until the MoQ session is fully initialized, so no additional
    /// `initialized()` check is needed.
    pub fn moq_handle<'a>(&'a mut self, quic: &'a mut quiche::Connection) -> Option<MoqHandle<'a>> {
        match &mut self.state {
            State::Moq { moq_session, h3_conn, wt_conn } => Some(MoqHandle::new(
                moq_session,
                quic,
                h3_conn,
                wt_conn,
            )),
            State::Native { session, streams } if session.initialized() => Some(MoqHandle { session, t: moq::QuicheTransport::raw(quic, streams) }),
            _ => None,
        }
    }

    /// Returns true if the MoQ session is ready for use
    pub fn is_ready(&self) -> bool {
        match &self.state {
            State::Moq { .. } => true,
            State::Native { session, .. } => session.initialized(),
            _ => false,
        }
    }

    /// Offers the ALPNs on `quic_config`: HTTP/3 for WebTransport, and for native QUIC the MOQT draft
    /// `moq_config` pins, if it names itself by ALPN (draft-ietf-moq-transport-18 §3.1.4).
    // TODO native QUIC of drafts 7 to 14, negotiated in band behind ALPN "moq-00", implement when needed
    pub fn set_application_protos(quic_config: &mut quiche::Config, moq_config: &moq::Config, webtransport: bool, native_quic: bool) -> Result<(), &'static str> {
        let native = match &moq_config.version_negotiation {
            moq::VersionNegotiation::Pinned(version) => version.alpn_protocol(),
            moq::VersionNegotiation::InBand { .. } => None,
        };
        if native_quic && native.is_none() { return Err("native QUIC needs a pinned draft 15 or later") }
        let webtransport = webtransport.then_some(ALPN_HTTP_3);
        let native = native.filter(|_| native_quic);
        let protos: Vec<&[u8]> = webtransport.into_iter().chain(native.as_deref().map(str::as_bytes)).collect();
        quic_config.set_application_protos(&protos).map_err(|_| "invalid ALPN")
    }

    /// The session on the QUIC connection itself, its draft named by the ALPN `protocol`, `None`
    /// while no stream can be opened yet.
    fn start_native(&self, protocol: &str, quic: &mut quiche::Connection) -> Option<State> {
        let version = moq::wire::Version::from_alpn_protocol(protocol).expect("offered by `set_application_protos`");
        let mut config = moq::Config { version_negotiation: moq::VersionNegotiation::Pinned(version), ..self.moq_config.clone() };
        let mut streams = moq::RawStreams::new(quic.is_server());
        let session = match &self.perspective {
            Perspective::Client { url } => {
                let path = match url.query() {
                    Some(query) => format!("{}?{query}", url.path()),
                    None => url.path().to_string(),
                };
                config.native_quic_uri = Some((url.authority().as_bytes().to_vec(), path.into_bytes()));
                match moq_core::MoqTransportSession::connect(&mut moq::QuicheTransport::raw(quic, &mut streams), config) {
                    Ok(session) => session,
                    Err(moq::Error::Done | moq::Error::InsufficientCapacity) => return None,
                    Err(e) => panic!("invalid moq_config: {e:?}"),
                }
            }
            Perspective::Server { .. } => moq_core::MoqTransportSession::accept(config).expect("invalid moq_config: VersionNegotiation"),
        };
        Some(State::Native { session, streams })
    }

    /// Must be called when new quic packets have been received or the timeout fired.
    pub fn on_post_handle_recvs(&mut self, quic_conn: &mut quiche::Connection) {
        'conn: loop {
            match &mut self.state {
                State::Quic => {
                    // server: SETTINGS in 0.5-RTT, as soon as the transport is ready (RFC 9114 §7.2.4.2)
                    if !quic_conn.can_send_app_data() {
                        break 'conn; // not ready for h3 yet
                    }
                    if matches!(self.perspective, Perspective::Server { native_quic: true }) && !quic_conn.is_established() {
                        break 'conn; // the ALPN tells native QUIC from h3
                    }
                    if let Ok(protocol) = std::str::from_utf8(quic_conn.application_proto()).map(str::to_owned)
                        && protocol.starts_with("moqt-")
                    {
                        let Some(native) = self.start_native(&protocol, quic_conn) else { break 'conn };
                        self.state = native;
                        continue;
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
                        wt_conn: wt::Connection::new(matches!(self.perspective, Perspective::Server { .. }))
                    }
                }
                State::H3 { h3_conn, wt_conn } => {
                    match &mut self.perspective {
                        Perspective::Client { url } => {
                            wt_conn.poll_h3(h3_conn, quic_conn);
                            if !wt::webtransport_enabled_by_server(h3_conn) {
                                break 'conn; // not ready for wt
                            }
                            // draft >= 15 negotiates via ALPN/WT-Available-Protocols,
                            // not CLIENT_SETUP's version list — offer it when pinned.
                            let available_protocols: Vec<String> = match &self.moq_config.version_negotiation {
                                moq::VersionNegotiation::Pinned(v) => v.alpn_protocol().into_iter().collect(),
                                moq::VersionNegotiation::InBand { .. } => vec![],
                            };
                            // stream limit reached: retried on the next call
                            let Ok(moq_session_id) = wt_conn.connect_session(h3_conn, quic_conn, url.clone(), &available_protocols) else {
                                break 'conn;
                            };
                            // a single offered version fixes the wire format: send CLIENT_SETUP
                            // with the CONNECT (draft-ietf-webtrans-http3-16 §4.2, §4.6)
                            let optimistic = available_protocols.len() == 1;
                            let moq_session = optimistic
                                .then(|| connect_moq(moq_session_id, h3_conn, quic_conn, wt_conn, &self.moq_config))
                                .flatten();
                            let State::H3 { h3_conn, wt_conn } = std::mem::take(&mut self.state) else {
                                unreachable!()
                            };
                            self.state = match moq_session {
                                Some(moq_session) => State::MoqHandshake { h3_conn, wt_conn, moq_session },
                                None => State::Wt { h3_conn, wt_conn, moq_session_id },
                            };
                        }
                        Perspective::Server { .. } => {
                            wt_conn.poll_h3(h3_conn, quic_conn);
                            let Some(&moq_session_id) = wt_conn.session_ids().first() else { break 'conn };
                            // If we're pinned and the client offered that same
                            // protocol, confirm it via `WT-Protocol`; otherwise
                            // fall back to whatever `self.moq_config` says.
                            if let moq::VersionNegotiation::Pinned(v) = self.moq_config.version_negotiation
                                && let Some(protocol) = v.alpn_protocol()
                                && wt_conn.pending_available_protocols(moq_session_id).contains(&protocol)
                            {
                                wt_conn.set_negotiated_protocol(moq_session_id, protocol);
                            }
                            let moq_session = MoqTransportSession::accept(moq_session_id.into(), self.moq_config.clone())
                                .expect("invalid moq_config: VersionNegotiation");
                            let State::H3 { h3_conn, wt_conn } = std::mem::take(&mut self.state) else {
                                unreachable!()
                            };
                            self.state = State::MoqHandshake {
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
                    wt_conn.poll_h3(h3_conn, quic_conn);
                    wt_conn.poll(h3_conn, quic_conn);
                    if !wt_conn.established(*moq_session_id) {
                        break 'conn; // not ready for moq
                    }
                    // The server must have echoed back what we offered (see
                    // `Connection::recv_hdrs`) — nothing more to do here.
                    if let (moq::VersionNegotiation::Pinned(v), Some(protocol)) =
                        (&self.moq_config.version_negotiation, wt_conn.negotiated_protocol(*moq_session_id))
                    {
                        debug!("WebTransport confirmed MoQT protocol {protocol} for pinned draft {v}");
                    }
                    let Some(moq_session) = connect_moq(*moq_session_id, h3_conn, quic_conn, wt_conn, &self.moq_config) else {
                        break 'conn;
                    };
                    let State::Wt {
                        h3_conn, wt_conn, ..
                    } = std::mem::take(&mut self.state)
                    else {
                        unreachable!()
                    };
                    self.state = State::MoqHandshake {
                        h3_conn,
                        wt_conn,
                        moq_session,
                    };
                }
                State::MoqHandshake {
                    h3_conn,
                    wt_conn,
                    moq_session,
                } => {
                    wt_conn.poll_h3(h3_conn, quic_conn);
                    wt_conn.poll(h3_conn, quic_conn);
                    moq_session.handle(wt_conn, h3_conn, quic_conn);
                    if !moq_session.session().initialized() {
                        break 'conn; // not ready for moq
                    }
                    let State::MoqHandshake {
                        h3_conn, wt_conn, moq_session, ..
                    } = std::mem::take(&mut self.state)
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
                    wt_conn.poll_h3(h3_conn, quic_conn);
                    wt_conn.poll(h3_conn, quic_conn);
                    moq_session.handle(wt_conn, h3_conn, quic_conn);
                    break 'conn;
                }
                State::Native { session, streams } => {
                    let readable: Vec<u64> = quic_conn.readable().collect();
                    session.handle(&mut moq::QuicheTransport::raw(quic_conn, streams), &readable, moq_core::Timestamp::now());
                    break 'conn;
                }
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

#[derive(Default)]
#[allow(clippy::large_enum_variant)]
pub enum State {
    #[default]
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
    MoqHandshake {
        h3_conn: h3::Connection,
        wt_conn: wt::Connection,
        moq_session: MoqTransportSession,
    },
    Moq {
        h3_conn: h3::Connection,
        wt_conn: wt::Connection,
        moq_session: MoqTransportSession,
    },
    /// On the QUIC connection itself (draft-ietf-moq-transport-18 §3.1.4), ready once initialized.
    Native {
        session: moq_core::MoqTransportSession,
        streams: moq::RawStreams,
    },
}

enum Perspective {
    Client {
        url: Url
    },
    Server {
        /// Offers native QUIC besides WebTransport, see `with_native_quic`
        native_quic: bool,
    },
}

/// Sends CLIENT_SETUP, `None` while no stream can be opened yet.
///
/// Panics on a `moq_config` that can't be put on the wire.
fn connect_moq(
    session_id: wt::SessionId,
    h3: &mut h3::Connection,
    quic: &mut quiche::Connection,
    wt: &mut wt::Connection,
    moq_config: &moq::Config,
) -> Option<MoqTransportSession> {
    match MoqTransportSession::connect(session_id.into(), h3, quic, wt, moq_config.clone()) {
        Ok(session) => Some(session),
        Err(moq::Error::Done | moq::Error::InsufficientCapacity) => None,
        Err(e) => panic!("invalid moq_config: {e:?}"),
    }
}

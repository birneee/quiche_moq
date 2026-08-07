//! Real UDP sockets + tokio-quiche, wiring `webtransport::WebTransportApp`
//! in as the connection driver instead of tokio-quiche's ready-made
//! (WebTransport-unaware) `H3Driver`.

use crate::webtransport::{new_pair, WebTransportHandle};
use crate::Error;
use moq_core_async::Notify;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio_quiche::quic::connect_with_config;
use tokio_quiche::settings::{ConnectionParams, Hooks, QuicSettings, TlsCertificatePaths};
use tokio_stream::StreamExt as _;
use url::Url;

fn client_settings() -> QuicSettings {
    let mut settings = QuicSettings::default();
    settings.verify_peer = false;
    settings
}

pub(crate) async fn connect(addr: SocketAddr) -> Result<(WebTransportHandle, Notify), Error> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.map_err(|_| Error)?;
    socket.connect(addr).await.map_err(|_| Error)?;

    let url = Url::parse(&format!("https://{addr}/")).map_err(|_| Error)?;
    let (app, handle, peer_wake) = new_pair(false, Some(url));

    let params = ConnectionParams::new_client(client_settings(), None, Hooks::default());
    let socket: tokio_quiche::socket::Socket<_, _> = socket.try_into().map_err(|_: std::io::Error| Error)?;
    connect_with_config(socket, None, &params, app)
        .await
        .map_err(|_| Error)?;

    Ok((handle, peer_wake))
}

/// A listening MoQ-over-WebTransport server socket, generating a fresh
/// self-signed cert on each call — fine for loopback testing, not meant for
/// production use (a real deployment would take `TlsCertificatePaths` in).
pub(crate) struct Listener {
    local_addr: SocketAddr,
    incoming: tokio_quiche::QuicConnectionStream<tokio_quiche::metrics::DefaultMetrics>,
    _cert_dir: TempDir,
}

/// Minimal directory-cleans-itself-up helper — avoids taking on the
/// `tempfile` crate for two files.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Listener {
    pub(crate) async fn bind(addr: &str) -> Result<Self, Error> {
        let socket = std::net::UdpSocket::bind(addr).map_err(|_| Error)?;
        socket.set_nonblocking(true).map_err(|_| Error)?;
        let local_addr = socket.local_addr().map_err(|_| Error)?;

        let dir = std::env::temp_dir().join(format!("moq_tokio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|_| Error)?;
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).map_err(|_| Error)?;
        std::fs::write(&cert_path, cert.cert.pem()).map_err(|_| Error)?;
        std::fs::write(&key_path, cert.key_pair.serialize_pem()).map_err(|_| Error)?;

        let params = ConnectionParams::new_server(
            QuicSettings::default(),
            TlsCertificatePaths {
                cert: cert_path.to_str().ok_or(Error)?,
                private_key: key_path.to_str().ok_or(Error)?,
                kind: Default::default(),
            },
            Hooks::default(),
        );
        // `listen`'s `params` is consumed by value per-call internally via
        // `Config::new`, but the function signature borrows it for the
        // duration of the call only, so this local `params` is fine to drop
        // after — the listener doesn't need it kept alive.
        let incoming = tokio_quiche::listen([socket], params, tokio_quiche::metrics::DefaultMetrics)
            .map_err(|_| Error)?
            .into_iter()
            .next()
            .ok_or(Error)?;

        Ok(Self { local_addr, incoming, _cert_dir: TempDir(dir) })
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub(crate) async fn accept(&mut self) -> Result<(WebTransportHandle, Notify), Error> {
        let conn = self.incoming.next().await.ok_or(Error)?.map_err(|_| Error)?;
        let (app, handle, peer_wake) = new_pair(true, None);
        conn.start(app);
        Ok((handle, peer_wake))
    }
}

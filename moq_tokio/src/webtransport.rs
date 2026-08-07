//! WebTransport-over-H3 `AsyncTransport`, built on `tokio-quiche`'s
//! `ApplicationOverQuic` (raw QUIC I/O driver) + this workspace's own
//! `quiche_webtransport` crate (extended CONNECT handshake, WT stream
//! framing — genericized over the buffer factory so it works with
//! tokio-quiche's connection type, see `~/Git/quiche_webtransport`'s
//! `webtransport_streams_fix-2452` branch).
//!
//! Bridges two tasks: tokio-quiche's worker task owns the real
//! `quiche::Connection`/`h3::Connection` and calls `process_reads`/
//! `process_writes` on it; the `moq_core::Transport` impl here is called
//! synchronously from a *different* task (the `AsyncMoqSessionActor`'s
//! driver loop) and only ever touches shared, `Mutex`-protected buffers —
//! same shape as `moq_web`'s `BrowserWebTransport`, just bridging two Rust
//! tasks instead of Rust and JS.

use moq_core::transport::StreamError;
use moq_core::Transport;
use moq_core_async::{AsyncTransport, Notify};
use quiche::h3;
use quiche_webtransport::Connection as WtConnection;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio_quiche::quic::{HandshakeInfo, QuicheConnection};
use tokio_quiche::{ApplicationOverQuic, QuicResult};
use url::Url;

struct Shared {
    wt: Mutex<WtConnection>,
    /// Set once the WT session is established (client: header response
    /// received; server: CONNECT accepted). `moq_core::Transport` doesn't
    /// have a concept of "not established yet" — callers must wait for
    /// this before using the transport, same as waiting for `wt.ready()`
    /// in `moq_web`.
    session_id: Mutex<Option<u64>>,
    /// Local (`moq_core`-visible) stream id -> real WT stream id.
    ///
    /// `moq_core::control_channel::is_bidi_stream` classifies stream ids by
    /// their low bits (bit0: 0=client/1=server-initiated, bit1: 0=bidi/
    /// 1=uni — real QUIC's own convention), so local ids can't be an
    /// arbitrary counter: for streams *we* open (`open_uni_stream`/
    /// `open_bidi_stream`), the local id is synthetic (the real id isn't
    /// known until `process_writes` actually opens it) but chosen to
    /// already carry the right low bits, using `next_self_bidi`/
    /// `next_self_uni` below — same scheme as `moq_web`'s
    /// `BrowserWebTransport`. For streams the *peer* opens (discovered in
    /// `process_reads`), the real id already has correct low bits (it's a
    /// real QUIC stream id), so it's used as the local id directly — no
    /// separate numbering needed.
    local_to_real: Mutex<HashMap<u64, u64>>,
    real_to_local: Mutex<HashMap<u64, u64>>,
    next_self_bidi: Mutex<u64>,
    next_self_uni: Mutex<u64>,
    /// Streams `open_uni_stream`/`open_bidi_stream` allocated a local id
    /// for but `process_writes` hasn't actually opened yet.
    pending_opens: Mutex<VecDeque<(u64, bool)>>,
    /// Bytes queued by `send_stream` before the real stream exists yet, or
    /// before `process_writes` has flushed them.
    pending_sends: Mutex<HashMap<u64, VecDeque<(Vec<u8>, bool)>>>,
    recv_bufs: Mutex<HashMap<u64, VecDeque<u8>>>,
    recv_fins: Mutex<HashSet<u64>>,
    /// Streams `recv_stream` has reported as fully drained and `Fin`'d —
    /// the session drops its `InStream` on seeing that, so
    /// `retire_finished_streams` can drop `recv_bufs`/`recv_fins`'s entries
    /// for them next cycle. Without this they'd accumulate forever (same
    /// fix as `moq_web`'s `BrowserWebTransport::recv_retired`).
    recv_retired: Mutex<HashSet<u64>>,
    /// Woken by `Transport` methods (new opens/sends queued); read by
    /// `wait_for_data` to trigger another `process_writes`.
    local_wake: Notify,
    /// Woken by `process_reads`/`process_writes` whenever new data (or the
    /// session becoming established) is available — this is the `Notify`
    /// `AsyncMoqSessionActor` waits on.
    peer_wake: Notify,
}

/// The `ApplicationOverQuic` tokio-quiche drives — owns the real H3/WT
/// connection state. Lives entirely inside tokio-quiche's worker task.
pub(crate) struct WebTransportApp {
    shared: Arc<Shared>,
    h3: Option<h3::Connection>,
    is_server: bool,
    url: Option<Url>,
    /// Scratch space the worker writes outbound packets into (see
    /// `ApplicationOverQuic::buffer`'s doc comment) — max UDP payload size.
    out_buf: Vec<u8>,
}

impl WebTransportApp {
    fn new(shared: Arc<Shared>, is_server: bool, url: Option<Url>) -> Self {
        Self { shared, h3: None, is_server, url, out_buf: vec![0u8; 65535] }
    }

    fn drain_pending_opens(&mut self, qconn: &mut QuicheConnection) {
        if self.shared.pending_opens.lock().unwrap().is_empty() {
            return;
        }
        let Some(h3) = &mut self.h3 else { return };
        let Some(session_id) = *self.shared.session_id.lock().unwrap() else { return };
        let mut wt = self.shared.wt.lock().unwrap();
        // `session_id` above is set as soon as we've *sent* CONNECT (client)
        // or *received* it (server) — `established()` is quiche_webtransport's
        // own bookkeeping, only true once the 200 response has actually been
        // processed (`recv_hdrs`). `open_stream` asserts on this, so wait for it.
        if !wt.established(session_id) {
            return;
        }
        let mut pending = self.shared.pending_opens.lock().unwrap();
        pending.retain(|&(local_id, bidi)| {
            match wt.open_stream(session_id, h3, qconn, bidi) {
                Ok(real_id) => {
                    self.shared.local_to_real.lock().unwrap().insert(local_id, real_id);
                    self.shared.real_to_local.lock().unwrap().insert(real_id, local_id);
                    false
                }
                Err(quiche_webtransport::Error::InsufficientCapacity) => true,
                Err(e) => {
                    log::error!("open_stream: {e:?}");
                    false
                }
            }
        });
    }

    fn drain_pending_sends(&mut self, qconn: &mut QuicheConnection) {
        if self.h3.is_none() {
            return;
        }
        if self.shared.pending_sends.lock().unwrap().values().all(VecDeque::is_empty) {
            return;
        }
        let mut wt = self.shared.wt.lock().unwrap();
        let local_to_real = self.shared.local_to_real.lock().unwrap();
        let mut pending_sends = self.shared.pending_sends.lock().unwrap();
        for (local_id, queue) in pending_sends.iter_mut() {
            let Some(&real_id) = local_to_real.get(local_id) else { continue };
            while let Some((data, fin)) = queue.pop_front() {
                match wt.stream_send(real_id, qconn, &data, fin) {
                    Ok(n) if n == data.len() => {}
                    Ok(n) => {
                        // partial send: requeue the remainder at the front and stop.
                        queue.push_front((data[n..].to_vec(), fin));
                        break;
                    }
                    Err(quiche_webtransport::Error::Done) => {
                        queue.push_front((data, fin));
                        break;
                    }
                    Err(e) => {
                        log::error!("stream_send on {real_id}: {e:?}");
                        break;
                    }
                }
            }
        }
    }
}

impl ApplicationOverQuic for WebTransportApp {
    fn on_conn_established(
        &mut self, qconn: &mut QuicheConnection, _handshake_info: &HandshakeInfo,
    ) -> QuicResult<()> {
        let mut config = h3::Config::new()?;
        quiche_webtransport::configure_h3(&mut config).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        let mut h3 = h3::Connection::with_transport(qconn, &config)?;
        if !self.is_server {
            let url = self.url.clone().expect("client requires a URL");
            let mut wt = self.shared.wt.lock().unwrap();
            let session_id = wt.connect_session(&mut h3, qconn, url, &[]);
            *self.shared.session_id.lock().unwrap() = Some(session_id);
        }
        self.h3 = Some(h3);
        Ok(())
    }

    fn should_act(&self) -> bool {
        true
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.out_buf
    }

    fn wait_for_data(
        &mut self, _qconn: &mut QuicheConnection,
    ) -> impl Future<Output = QuicResult<()>> + Send {
        let wake = self.shared.local_wake.clone();
        async move {
            wake.notified().await;
            Ok(())
        }
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        let Some(mut h3) = self.h3.take() else { return Ok(()) };

        loop {
            match h3.poll(qconn) {
                Ok((stream_id, h3::Event::Headers { list, .. })) => {
                    self.shared.wt.lock().unwrap().recv_hdrs(stream_id, &list);
                    if self.is_server {
                        // Server: a CONNECT just got accepted into a session — the
                        // response itself is sent by `WtConnection::poll` below.
                        if self.shared.wt.lock().unwrap().established(stream_id) {
                            *self.shared.session_id.lock().unwrap() = Some(stream_id);
                        }
                    } else if self.shared.wt.lock().unwrap().established(stream_id) {
                        self.shared.peer_wake.notify();
                    }
                }
                Ok((_, h3::Event::Data)) | Ok((_, h3::Event::Finished)) => {}
                Ok((_, h3::Event::Reset(_))) => {}
                Ok(_) => {}
                Err(h3::Error::Done) => break,
                Err(e) => {
                    log::error!("h3 poll: {e:?}");
                    break;
                }
            }
        }

        {
            let mut wt = self.shared.wt.lock().unwrap();
            wt.poll(&mut h3, qconn);

            for stream_id in wt.readable_sessions() {
                for real_id in wt.readable_streams(stream_id) {
                    // The peer opened this — its real id already carries the
                    // low bits `moq_core` needs (see `Shared::local_to_real`'s
                    // doc comment), so it's used as the local id directly.
                    let local_id = real_id;
                    self.shared.real_to_local.lock().unwrap().entry(real_id).or_insert(local_id);
                    self.shared.local_to_real.lock().unwrap().entry(local_id).or_insert(real_id);
                    let mut buf = [0u8; 64 << 10];
                    loop {
                        match wt.recv_stream(real_id, stream_id, &mut h3, qconn, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                self.shared
                                    .recv_bufs
                                    .lock()
                                    .unwrap()
                                    .entry(local_id)
                                    .or_default()
                                    .extend(&buf[..n]);
                            }
                            Err(quiche_webtransport::Error::Done) => break,
                            Err(quiche_webtransport::Error::Fin) => {
                                self.shared.recv_fins.lock().unwrap().insert(local_id);
                                break;
                            }
                            Err(e) => {
                                log::error!("recv_stream on {real_id}: {e:?}");
                                break;
                            }
                        }
                    }
                }
            }
        }

        self.h3 = Some(h3);
        self.shared.peer_wake.notify();
        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        self.drain_pending_opens(qconn);
        self.drain_pending_sends(qconn);
        Ok(())
    }
}

/// Handle for the `AsyncMoqSessionActor`'s task — implements
/// `moq_core::Transport`/`AsyncTransport` purely against shared state, never
/// touching the real QUIC connection directly (that only ever happens
/// inside `WebTransportApp`, on tokio-quiche's own task).
#[derive(Clone)]
pub struct WebTransportHandle {
    shared: Arc<Shared>,
}

pub(crate) fn new_pair(is_server: bool, url: Option<Url>) -> (WebTransportApp, WebTransportHandle, Notify) {
    let peer_wake = Notify::new();
    // QUIC stream id parity: bit0 = 0 client / 1 server, bit1 = 0 bidi / 1 uni.
    let (next_self_bidi, next_self_uni) = if is_server { (1, 3) } else { (0, 2) };
    let shared = Arc::new(Shared {
        wt: Mutex::new(WtConnection::new(is_server)),
        session_id: Mutex::new(None),
        local_to_real: Mutex::new(HashMap::new()),
        real_to_local: Mutex::new(HashMap::new()),
        next_self_bidi: Mutex::new(next_self_bidi),
        next_self_uni: Mutex::new(next_self_uni),
        pending_opens: Mutex::new(VecDeque::new()),
        pending_sends: Mutex::new(HashMap::new()),
        recv_bufs: Mutex::new(HashMap::new()),
        recv_fins: Mutex::new(HashSet::new()),
        recv_retired: Mutex::new(HashSet::new()),
        local_wake: Notify::new(),
        peer_wake: peer_wake.clone(),
    });
    let app = WebTransportApp::new(shared.clone(), is_server, url);
    let handle = WebTransportHandle { shared };
    (app, handle, peer_wake)
}

impl Transport for WebTransportHandle {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError> {
        let mut recv_bufs = self.shared.recv_bufs.lock().unwrap();
        let Some(stream) = recv_bufs.get_mut(&stream_id) else {
            return Err(StreamError::Done);
        };
        if stream.is_empty() {
            return if self.shared.recv_fins.lock().unwrap().contains(&stream_id) {
                self.shared.recv_retired.lock().unwrap().insert(stream_id);
                Err(StreamError::Fin)
            } else {
                Err(StreamError::Done)
            };
        }
        let n = stream.len().min(buf.len());
        for (i, b) in stream.drain(..n).enumerate() {
            buf[i] = b;
        }
        Ok(n)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        self.shared
            .pending_sends
            .lock()
            .unwrap()
            .entry(stream_id)
            .or_default()
            .push_back((data.to_vec(), fin));
        self.shared.local_wake.notify();
        Ok(data.len())
    }

    fn send_stream_if_capacity(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        self.send_stream(stream_id, data, fin)
    }

    fn open_uni_stream(&mut self) -> Result<u64, StreamError> {
        let mut next = self.shared.next_self_uni.lock().unwrap();
        let id = *next;
        *next += 4;
        self.shared.pending_opens.lock().unwrap().push_back((id, false));
        self.shared.local_wake.notify();
        Ok(id)
    }

    fn open_bidi_stream(&mut self) -> Result<u64, StreamError> {
        let mut next = self.shared.next_self_bidi.lock().unwrap();
        let id = *next;
        *next += 4;
        self.shared.pending_opens.lock().unwrap().push_back((id, true));
        self.shared.local_wake.notify();
        Ok(id)
    }

    fn set_stream_priority(&mut self, _stream_id: u64, _urgency: u8, _incremental: bool) {}
    fn reset_stream(&mut self, _stream_id: u64, _error_code: u64) {}
    fn stop_sending(&mut self, _stream_id: u64, _error_code: u64) {}
    fn close_session(&mut self, _error_code: u64, _reason: &str) {}
}

impl AsyncTransport for WebTransportHandle {
    type FlushError = std::convert::Infallible;

    fn readable_stream_ids(&self) -> Vec<u64> {
        self.shared.recv_bufs.lock().unwrap().keys().copied().collect()
    }

    fn retire_finished_streams(&mut self) {
        let mut retired = self.shared.recv_retired.lock().unwrap();
        if retired.is_empty() {
            return;
        }
        let mut recv_bufs = self.shared.recv_bufs.lock().unwrap();
        let mut recv_fins = self.shared.recv_fins.lock().unwrap();
        for stream_id in retired.drain() {
            recv_bufs.remove(&stream_id);
            recv_fins.remove(&stream_id);
        }
    }

    async fn flush_sends(&mut self) -> Result<(), std::convert::Infallible> {
        // Sends are handed to `WebTransportApp` via `pending_sends` and
        // actually written to the wire on tokio-quiche's own task
        // (`process_writes`) — nothing more to do on this side.
        Ok(())
    }

    type ConnectParams = std::net::SocketAddr;
    type ConnectError = crate::Error;

    async fn connect(addr: std::net::SocketAddr) -> Result<(Self, moq_core_async::Notify), crate::Error> {
        crate::net::connect(addr).await
    }
}

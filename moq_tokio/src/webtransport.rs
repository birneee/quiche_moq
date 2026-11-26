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
use moq_core::wire::{ResetCode, SessionCloseCode};
use moq_core_async::{AsyncTransport, Notify};
use quiche::h3;
use quiche_webtransport::{is_uni, Connection as WtConnection};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio_quiche::quic::{HandshakeInfo, QuicheConnection};
use tokio_quiche::{ApplicationOverQuic, QuicResult};
use url::Url;

/// How a received stream ended.
#[derive(Clone, Copy)]
enum RecvEnd {
    Fin,
    Reset(u64),
}


impl From<RecvEnd> for StreamError {
    fn from(end: RecvEnd) -> Self {
        match end {
            RecvEnd::Fin => StreamError::Fin,
            RecvEnd::Reset(code) => StreamError::StreamReset(code),
        }
    }
}

/// A stream's writes not passed to quiche yet, with their FIN.
type SendQueue = VecDeque<(Vec<u8>, bool)>;

/// Everything both tasks touch, behind one lock.
struct State {
    wt: WtConnection,
    /// Set once the WT session is established (client: header response
    /// received; server: CONNECT accepted). `moq_core::Transport` doesn't
    /// have a concept of "not established yet" — callers must wait for
    /// this before using the transport, same as waiting for `wt.ready()`
    /// in `moq_web`.
    session_id: Option<u64>,
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
    local_to_real: HashMap<u64, u64>,
    real_to_local: HashMap<u64, u64>,
    next_self_bidi: u64,
    next_self_uni: u64,
    /// Streams `open_uni_stream`/`open_bidi_stream` allocated a local id
    /// for but `process_writes` hasn't actually opened yet.
    pending_opens: VecDeque<(u64, bool)>,
    /// Bytes queued by `send_stream` before the real stream exists yet, or
    /// before `process_writes` has flushed them.
    pending_sends: HashMap<u64, SendQueue>,
    /// Streams `reset_stream` asked to reset, with the code, applied by `process_writes`.
    pending_resets: Vec<(u64, u32)>,
    /// Datagrams `send_datagram` queued, sent by `process_writes`.
    pending_datagrams: Vec<Vec<u8>>,
    /// Code and reason `close_session` queued, sent by `process_writes` after the data before it.
    pending_close: Option<(u32, String)>,
    /// Our uni streams whose FIN quiche has delivered, reported once by `stream_finished`.
    send_finished: HashSet<u64>,
    /// Bidi streams the peer sent STOP_SENDING for, reported by `stream_stopped`.
    stopped: HashSet<u64>,
    recv_bufs: HashMap<u64, VecDeque<u8>>,
    /// How each received stream ended, reported once its bytes are read.
    recv_ends: HashMap<u64, RecvEnd>,
    /// Streams `recv_stream` has reported as fully drained and ended —
    /// the session drops its `InStream` on seeing that, so
    /// `retire_finished_streams` can drop `recv_bufs`/`recv_ends`'s entries
    /// for them next cycle. Without this they'd accumulate forever (same
    /// fix as `moq_web`'s `BrowserWebTransport::recv_retired`).
    recv_retired: HashSet<u64>,
}

impl State {
    fn new(is_server: bool) -> Self {
        // QUIC stream id parity: bit0 = 0 client / 1 server, bit1 = 0 bidi / 1 uni.
        let (next_self_bidi, next_self_uni) = if is_server { (1, 3) } else { (0, 2) };
        Self {
            wt: WtConnection::new(is_server),
            session_id: None,
            local_to_real: HashMap::new(),
            real_to_local: HashMap::new(),
            next_self_bidi,
            next_self_uni,
            pending_opens: VecDeque::new(),
            pending_sends: HashMap::new(),
            pending_resets: Vec::new(),
            pending_datagrams: Vec::new(),
            pending_close: None,
            send_finished: HashSet::new(),
            stopped: HashSet::new(),
            recv_bufs: HashMap::new(),
            recv_ends: HashMap::new(),
            recv_retired: HashSet::new(),
        }
    }

    fn forget_stream(&mut self, local_id: u64, real_id: u64) {
        self.stopped.remove(&local_id);
        self.local_to_real.remove(&local_id);
        self.real_to_local.remove(&real_id);
    }

    fn open_stream(&mut self, bidi: bool) -> u64 {
        let next = if bidi { &mut self.next_self_bidi } else { &mut self.next_self_uni };
        let id = *next;
        *next += 4;
        self.pending_opens.push_back((id, bidi));
        id
    }

    fn drain_pending_opens(&mut self, h3: &mut h3::Connection, qconn: &mut QuicheConnection) {
        // `session_id` is set as soon as we've *sent* CONNECT (client) or
        // *received* it (server) — `established()` is quiche_webtransport's
        // own bookkeeping, only true once the 200 response has actually been
        // processed (`recv_hdrs`). `open_stream` asserts on this, so wait for it.
        let Some(session_id) = self.session_id.filter(|&id| self.wt.established(id)) else { return };
        let Self { wt, pending_opens, local_to_real, real_to_local, .. } = self;
        pending_opens.retain(|&(local_id, bidi)| {
            match wt.open_stream(session_id, h3, qconn, bidi) {
                Ok(real_id) => {
                    local_to_real.insert(local_id, real_id);
                    real_to_local.insert(real_id, local_id);
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

    fn drain_pending_sends(&mut self, h3: &mut h3::Connection, qconn: &mut QuicheConnection, fin_written: &mut HashMap<u64, u64>) {
        let Self { wt, pending_sends, local_to_real, .. } = self;
        pending_sends.retain(|local_id, queue| {
            let Some(&real_id) = local_to_real.get(local_id) else { return true };
            while let Some((data, fin)) = queue.pop_front() {
                match wt.stream_send(real_id, h3, qconn, &data, fin) {
                    Ok(n) if n == data.len() => {
                        if fin {
                            fin_written.insert(real_id, *local_id);
                        }
                    }
                    Ok(n) => {
                        // partial send: requeue the remainder at the front and stop.
                        queue.push_front((data[n..].to_vec(), fin));
                        break;
                    }
                    Err(quiche_webtransport::Error::Done) => {
                        queue.push_front((data, fin));
                        break;
                    }
                    // e.g. stopped by the peer: nothing more can be sent
                    Err(e) => {
                        log::error!("stream_send on {real_id}: {e:?}");
                        queue.clear();
                    }
                }
            }
            !queue.is_empty()
        });
    }

    /// Sends the queued datagrams, a datagram that doesn't fit is dropped (draft-ietf-moq-transport-18 §11.3).
    /// CLOSE_WEBTRANSPORT_SESSION (draft-ietf-moq-transport-18 §3.5).
    fn send_pending_close(&mut self, h3: &mut h3::Connection, qconn: &mut QuicheConnection) {
        if let (Some((code, reason)), Some(session_id)) = (self.pending_close.take(), self.session_id) {
            self.wt.close_session(session_id, code, &reason, qconn, h3).ok();
        }
    }

    fn drain_pending_datagrams(&mut self, qconn: &mut QuicheConnection) {
        let Some(session_id) = self.session_id else { return };
        for datagram in std::mem::take(&mut self.pending_datagrams) {
            if let Err(e) = self.wt.send_datagram(session_id, qconn, &datagram) {
                log::debug!("dropping a datagram: {e:?}");
            }
        }
    }

    fn drain_pending_resets(&mut self, h3: &mut h3::Connection, qconn: &mut QuicheConnection, fin_written: &mut HashMap<u64, u64>) {
        for (local_id, code) in std::mem::take(&mut self.pending_resets) {
            self.pending_sends.remove(&local_id);
            self.pending_opens.retain(|&(id, _)| id != local_id);
            let Some(&real_id) = self.local_to_real.get(&local_id) else { continue };
            fin_written.remove(&real_id);
            self.wt.shutdown_stream(real_id, quiche::Shutdown::Write, code, h3, qconn).ok();
            self.wt.stream_send_ended(real_id);
            if is_uni(local_id) {
                self.forget_stream(local_id, real_id);
            }
        }
    }

    /// Reports our uni streams whose FIN quiche delivered (`stream_finished`)
    /// and forgets them.
    fn check_finished_streams(&mut self, qconn: &mut QuicheConnection, fin_written: &mut HashMap<u64, u64>) {
        fin_written.retain(|&real_id, &mut local_id| {
            // a local uni stream counts as finished once quiche collected it, after the FIN is acked
            if !qconn.stream_finished(real_id) {
                return true;
            }
            if is_uni(local_id) {
                self.send_finished.insert(local_id);
                self.forget_stream(local_id, real_id);
            }
            false
        });
    }

    /// Notes the bidi streams the peer stopped, uni ones are only written to and see it then.
    fn note_stopped_streams(&mut self, qconn: &mut QuicheConnection) {
        for (&local_id, &real_id) in &self.local_to_real {
            if !is_uni(local_id) && matches!(qconn.stream_capacity(real_id), Err(quiche::Error::StreamStopped(_))) {
                self.stopped.insert(local_id);
            }
        }
    }

    /// Buffers everything readable on the peer's streams.
    fn read_streams(&mut self, h3: &mut h3::Connection, qconn: &mut QuicheConnection) {
        let mut buf = [0u8; 64 << 10];
        for session_id in self.wt.readable_sessions() {
            for real_id in self.wt.readable_streams(session_id) {
                // A stream the peer opened keeps its real id, its low bits are
                // what `moq_core` needs (see `State::local_to_real`'s doc comment)
                let local_id = *self.real_to_local.entry(real_id).or_insert(real_id);
                self.local_to_real.entry(local_id).or_insert(real_id);
                // reported to the session even if it ends without data
                let recv_buf = self.recv_bufs.entry(local_id).or_default();
                let end = loop {
                    match self.wt.recv_stream(real_id, session_id, h3, qconn, &mut buf) {
                        Ok(0) | Err(quiche_webtransport::Error::Done) => break None,
                        Ok(n) => recv_buf.extend(&buf[..n]),
                        Err(quiche_webtransport::Error::Fin) => break Some(RecvEnd::Fin),
                        Err(quiche_webtransport::Error::StreamReset(code)) => break Some(RecvEnd::Reset(code)),
                        // nothing more will arrive: report it as reset, not complete
                        Err(e) => {
                            log::error!("recv_stream on {real_id}: {e:?}");
                            break Some(RecvEnd::Reset(0));
                        }
                    }
                };
                if let Some(end) = end {
                    self.recv_ends.insert(local_id, end);
                }
            }
        }
    }
}

struct Shared {
    state: Mutex<State>,
    /// Woken by `Transport` methods (new opens/sends queued); read by
    /// `wait_for_data` to trigger another `process_writes`.
    local_wake: Notify,
    /// Woken by `process_reads`/`process_writes` whenever new data (or the
    /// session becoming established) is available — this is the `Notify`
    /// `AsyncMoqSessionActor` waits on.
    peer_wake: Notify,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

/// The `ApplicationOverQuic` tokio-quiche drives — owns the real H3/WT
/// connection state. Lives entirely inside tokio-quiche's worker task.
pub(crate) struct WebTransportApp {
    shared: Arc<Shared>,
    h3: Option<h3::Connection>,
    is_server: bool,
    url: Option<Url>,
    /// Real -> local id of our streams whose FIN is written, until it is delivered.
    fin_written: HashMap<u64, u64>,
}

impl ApplicationOverQuic for WebTransportApp {
    fn on_conn_established(
        &mut self, qconn: &mut QuicheConnection, _handshake_info: &HandshakeInfo,
    ) -> QuicResult<()> {
        let mut config = h3::Config::new()?;
        quiche_webtransport::configure_h3(&mut config).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        self.h3 = Some(h3::Connection::with_transport(qconn, &config)?);
        Ok(())
    }

    fn should_act(&self) -> bool {
        true
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
        let Some(h3) = &mut self.h3 else { return Ok(()) };
        let mut state = self.shared.state();
        let session_id = state.session_id;
        state.wt.poll_h3(h3, qconn);
        if self.is_server && session_id.is_none() {
            // a CONNECT got accepted, the response is sent by `poll` below
            state.session_id = state.wt.session_ids().first().copied();
        }
        // the CONNECT once the server's SETTINGS said it takes WebTransport (draft-ietf-webtrans-http3-16 §4.6)
        if !self.is_server && session_id.is_none() && quiche_webtransport::webtransport_enabled_by_server(h3) {
            let url = self.url.clone().expect("client requires a URL");
            let session_id = state.wt.connect_session(h3, qconn, url, &[]).map_err(|e| format!("WebTransport CONNECT: {e:?}"))?;
            state.session_id = Some(session_id);
        }
        state.wt.poll(h3, qconn);
        state.read_streams(h3, qconn);
        state.note_stopped_streams(qconn);
        drop(state);
        self.shared.peer_wake.notify();
        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        let Some(h3) = &mut self.h3 else { return Ok(()) };
        let mut state = self.shared.state();
        state.drain_pending_opens(h3, qconn);
        state.drain_pending_resets(h3, qconn, &mut self.fin_written);
        state.drain_pending_sends(h3, qconn, &mut self.fin_written);
        state.drain_pending_datagrams(qconn);
        state.send_pending_close(h3, qconn);
        state.check_finished_streams(qconn, &mut self.fin_written);
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
    let shared = Arc::new(Shared { state: Mutex::new(State::new(is_server)), local_wake: Notify::new(), peer_wake: peer_wake.clone() });
    let app = WebTransportApp { shared: shared.clone(), h3: None, is_server, url, fin_written: HashMap::new() };
    (app, WebTransportHandle { shared }, peer_wake)
}

impl WebTransportHandle {
    /// Runs `f` on the state, then wakes `WebTransportApp` to write.
    fn queue<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let result = f(&mut self.shared.state());
        self.shared.local_wake.notify();
        result
    }
}

impl Transport for WebTransportHandle {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError> {
        let mut state = self.shared.state();
        let state = &mut *state;
        let Some(stream) = state.recv_bufs.get_mut(&stream_id) else {
            return Err(StreamError::Done);
        };
        if stream.is_empty() {
            let Some(&end) = state.recv_ends.get(&stream_id) else {
                return Err(StreamError::Done);
            };
            state.recv_retired.insert(stream_id);
            return Err(end.into());
        }
        let n = stream.len().min(buf.len());
        for (b, byte) in buf.iter_mut().zip(stream.drain(..n)) {
            *b = byte;
        }
        Ok(n)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        self.queue(|state| state.pending_sends.entry(stream_id).or_default().push_back((data.to_vec(), fin)));
        Ok(data.len())
    }

    fn send_stream_if_capacity(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        self.send_stream(stream_id, data, fin)
    }

    fn open_uni_stream(&mut self) -> Result<u64, StreamError> {
        Ok(self.queue(|state| state.open_stream(false)))
    }

    fn open_bidi_stream(&mut self) -> Result<u64, StreamError> {
        Ok(self.queue(|state| state.open_stream(true)))
    }

    fn set_stream_priority(&mut self, _stream_id: u64, _urgency: u8, _incremental: bool) {}

    fn reset_stream(&mut self, stream_id: u64, code: ResetCode) {
        self.queue(|state| state.pending_resets.push((stream_id, code.to_u32())));
    }

    /// Ignored, the session drains the stream until it ends.
    fn stop_sending(&mut self, _stream_id: u64, _code: ResetCode) -> bool {
        false
    }
    fn stream_stopped(&mut self, stream_id: u64) -> bool {
        self.shared.state().stopped.contains(&stream_id)
    }
    fn send_datagram(&mut self, datagram: &[u8]) -> Result<(), StreamError> {
        self.queue(|state| state.pending_datagrams.push(datagram.to_vec()));
        Ok(())
    }

    fn recv_datagram(&mut self) -> Option<Vec<u8>> {
        let mut state = self.shared.state();
        let session_id = state.session_id?;
        state.wt.recv_datagram(session_id)
    }

    fn close_session(&mut self, code: SessionCloseCode, reason: &str) {
        self.queue(|state| state.pending_close = Some((code.into(), reason.to_owned())));
    }

    /// True once for a uni stream whose FIN was delivered, the session forgets it then.
    fn stream_finished(&self, stream_id: u64) -> bool {
        self.shared.state().send_finished.remove(&stream_id)
    }
}

impl AsyncTransport for WebTransportHandle {
    type DropQueue = moq_core::SharedDropQueue;
    type FlushError = std::convert::Infallible;

    fn readable_stream_ids(&self) -> Vec<u64> {
        self.shared.state().recv_bufs.keys().copied().collect()
    }

    fn retire_finished_streams(&mut self) {
        let mut state = self.shared.state();
        for stream_id in std::mem::take(&mut state.recv_retired) {
            state.recv_bufs.remove(&stream_id);
            state.recv_ends.remove(&stream_id);
            // a peer uni stream: local and real id are the same
            if is_uni(stream_id) {
                state.forget_stream(stream_id, stream_id);
            }
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

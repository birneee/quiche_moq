//! `moq_core::Transport` implementation bridging the synchronous MoQ session
//! state machine to the browser's async `WebTransport` streams. Reads are
//! pre-buffered by background reader tasks (spawned here and in `lib.rs`)
//! into `recv_bufs`/`recv_ends`; writes are queued in `send_bufs` and
//! flushed with `flush_sends`.

use crate::util::js_bool;
use moq_core::{StreamError, Transport};
use moq_core::wire::{ResetCode, SessionCloseCode};
use moq_core_async::{AsyncTransport, Notify};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::js_sys::{ArrayBuffer, Reflect, Uint8Array};
use web_sys::{
    ReadableStream, ReadableStreamByobReader, ReadableStreamDefaultReader,
    ReadableStreamGetReaderOptions, ReadableStreamReaderMode, WebTransport,
    WebTransportBidirectionalStream, WebTransportCloseInfo, WritableStream, WritableStreamDefaultWriter,
};

/// The control stream: for draft < 17 this is a single shared bidi stream
/// (the first one `open_bidi_stream` allocates); for draft 17+ it's our half
/// of the uni pair (the first one `open_uni_stream` allocates). Either way
/// it's the first stream `MoqTransportSession::connect` opens.
pub(crate) const CTRL_STREAM_ID: u64 = 0;

pub(crate) type SharedRecvBufs = Rc<RefCell<HashMap<u64, Vec<u8>>>>;
/// How each received stream ended, reported once its bytes are read.
pub(crate) type SharedRecvEnds = Rc<RefCell<HashMap<u64, RecvEnd>>>;

/// How a received stream ended.
#[derive(Clone, Copy)]
pub(crate) enum RecvEnd {
    Fin,
    /// the browser doesn't expose the reset code
    Reset,
}
type SharedWriters = Rc<RefCell<Writers>>;

/// Writers of our streams, registered once their async creation resolves.
#[derive(Default)]
struct Writers {
    open: HashMap<u64, WritableStreamDefaultWriter>,
    /// Reset before their writer existed.
    reset: HashSet<u64>,
    /// Stopped by the peer (STOP_SENDING), reported by `stream_stopped`.
    stopped: HashSet<u64>,
}

/// Keeps `writer`, or aborts it if its stream was reset meanwhile. Its `closed` rejecting while it is still
/// open, neither closed nor aborted by us, is the peer's STOP_SENDING.
fn register_writer(writers: &SharedWriters, stream_id: u64, writer: WritableStreamDefaultWriter, wake: &Notify) {
    if writers.borrow_mut().reset.remove(&stream_id) {
        let _ = writer.abort();
        return;
    }
    let closed = writer.closed();
    writers.borrow_mut().open.insert(stream_id, writer);
    let (writers, wake) = (writers.clone(), wake.clone());
    spawn_local(async move {
        if JsFuture::from(closed).await.is_err() {
            let mut writers = writers.borrow_mut();
            if writers.open.contains_key(&stream_id) {
                writers.stopped.insert(stream_id);
                wake.notify();
            }
        }
    });
}

/// Scratch buffer size for BYOB reads. Reused across reads on the same
/// stream, so this is a one-time allocation per stream, not per chunk. Sized
/// well above a typical keyframe object so large objects don't need several
/// separate `read()` calls (each a renderer/browser-process IPC round-trip).
const READ_CHUNK_SIZE: u32 = 1024 * 1024;

/// Spawn a background task that reads `stream` to completion, buffering bytes
/// into `recv_bufs[stream_id]` and marking `recv_ends` on EOF or reset, waking the
/// session loop after each read. Used for both the pre-opened control stream
/// (`BrowserWebTransport::spawn_reader`) and each newly accepted incoming uni
/// stream (`lib.rs`'s stream acceptor task) — a stream is a stream once it has
/// a `stream_id`, whether it's the control stream or a data one.
///
/// Uses a BYOB reader with a single reused scratch `ArrayBuffer` instead of
/// the default reader: the default reader has the browser allocate a fresh
/// `Uint8Array` internally on every chunk, whereas BYOB lets us hand it the
/// same buffer back each time, so that allocation happens once per stream
/// instead of once per chunk. Bytes are still safely copied out into the
/// Rust-side `Vec` via `Uint8Array::copy_to` — this isn't a zero-copy path
/// into wasm linear memory, which would need `unsafe` (`Uint8Array::view_mut`
/// aliases wasm memory, and is unsound here since reads are async and other
/// concurrently-running tasks can trigger memory growth while one is
/// in flight).
pub(crate) fn spawn_stream_reader(
    recv_bufs: SharedRecvBufs,
    recv_ends: SharedRecvEnds,
    stream_id: u64,
    stream: ReadableStream,
    wake: Notify,
) {
    spawn_local(async move {
        let options = ReadableStreamGetReaderOptions::new();
        options.set_mode(ReadableStreamReaderMode::Byob);
        let reader: ReadableStreamByobReader = match stream
            .get_reader_with_options(&options)
            .dyn_into()
        {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut buffer = ArrayBuffer::new(READ_CHUNK_SIZE);
        loop {
            let view = Uint8Array::new_with_byte_offset_and_length(&buffer, 0, READ_CHUNK_SIZE);
            let end = |end| {
                recv_ends.borrow_mut().insert(stream_id, end);
                wake.notify();
            };
            // a failed read is the stream being reset: not complete
            let Ok(res) = JsFuture::from(reader.read_with_array_buffer_view(&view)).await else {
                end(RecvEnd::Reset);
                break;
            };
            if js_bool(&res, "done") {
                end(RecvEnd::Fin);
                break;
            }
            let Some(arr) = Reflect::get(&res, &"value".into())
                .ok()
                .and_then(|v| v.dyn_into::<Uint8Array>().ok())
            else {
                end(RecvEnd::Reset);
                break;
            };

            let n = arr.length() as usize;
            {
                let mut bufs = recv_bufs.borrow_mut();
                let dest = bufs.entry(stream_id).or_default();
                let old_len = dest.len();
                dest.resize(old_len + n, 0);
                arr.copy_to(&mut dest[old_len..]);
            }
            wake.notify();

            // Reclaim the underlying buffer for the next read.
            buffer = arr.buffer();
        }
    });
}

pub(crate) struct BrowserWebTransport {
    pub(crate) recv_bufs: SharedRecvBufs,
    pub(crate) recv_ends: SharedRecvEnds,
    /// Streams whose end (FIN or reset) has been reported to the session. Their bookkeeping is
    /// dropped in `retire_finished_streams`; without that they would stay in
    /// `recv_bufs` forever and the session would keep re-creating an `InStream`
    /// for each of them on every wake.
    recv_retired: HashSet<u64>,
    /// Queued bytes per stream, and whether a FIN follows them.
    send_bufs: HashMap<u64, (Vec<u8>, bool)>,
    /// Streams closed (FIN) by `flush_sends`, reported once by `stream_finished`.
    /// - the browser doesn't report delivery, a closed writer is the last thing we see
    send_finished: RefCell<HashSet<u64>>,
    /// Shared with the async tasks spawned by `open_uni_stream`/`open_bidi_stream`,
    /// which register a writer here once the (necessarily async) JS-side stream
    /// creation resolves. Until then, `flush_sends` just leaves queued bytes for
    /// that stream id in `send_bufs` and retries on the next call.
    writers: SharedWriters,
    pub(crate) wt: WebTransport,
    wake: Notify,
    /// Client-initiated bidi stream ids: 0, 4, 8, ... (QUIC stream id scheme:
    /// bit0=0 client, bit1=0 bidi).
    next_bidi_id: u64,
    /// Client-initiated uni stream ids: 2, 6, 10, ... (bit0=0 client, bit1=1 uni).
    next_uni_id: u64,
}

impl BrowserWebTransport {
    pub(crate) fn new(wt: WebTransport, wake: Notify) -> Self {
        Self {
            recv_bufs: Rc::new(RefCell::new(HashMap::new())),
            recv_ends: Rc::new(RefCell::new(HashMap::new())),
            recv_retired: HashSet::new(),
            send_bufs: HashMap::new(),
            send_finished: RefCell::new(HashSet::new()),
            writers: Rc::default(),
            wt,
            wake,
            next_bidi_id: 0,
            next_uni_id: 2,
        }
    }

    /// Drop bookkeeping for streams already reported as finished, so the
    /// session stops seeing them as readable.
    pub(crate) fn retire_finished_streams(&mut self) {
        if self.recv_retired.is_empty() {
            return;
        }
        let mut recv_bufs = self.recv_bufs.borrow_mut();
        let mut recv_ends = self.recv_ends.borrow_mut();
        for stream_id in self.recv_retired.drain() {
            recv_bufs.remove(&stream_id);
            recv_ends.remove(&stream_id);
        }
    }

    pub(crate) async fn flush_sends(&mut self) -> Result<(), JsValue> {
        let pending = std::mem::take(&mut self.send_bufs);
        for (stream_id, (data, fin)) in pending {
            let writer = self.writers.borrow().open.get(&stream_id).cloned();
            match writer {
                Some(writer) => {
                    if !data.is_empty() {
                        let arr = Uint8Array::from(data.as_slice());
                        JsFuture::from(writer.write_with_chunk(&arr)).await?;
                    }
                    if fin {
                        self.writers.borrow_mut().open.remove(&stream_id);
                        JsFuture::from(writer.close()).await?;
                        self.send_finished.borrow_mut().insert(stream_id);
                    }
                }
                // JS-side stream creation (spawned by open_uni_stream/open_bidi_stream)
                // hasn't resolved yet — requeue for the next flush.
                None => {
                    let queued = self.send_bufs.entry(stream_id).or_default();
                    queued.0.extend(data);
                    queued.1 |= fin;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn readable_stream_ids(&self) -> Vec<u64> {
        self.recv_bufs.borrow().keys().copied().collect()
    }
}

impl Transport for BrowserWebTransport {
    fn recv_stream(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<usize, StreamError> {
        let mut recv_bufs = self.recv_bufs.borrow_mut();
        let stream_buf = match recv_bufs.get_mut(&stream_id) {
            None => return Err(StreamError::Done),
            Some(b) => b,
        };
        if stream_buf.is_empty() {
            let Some(&end) = self.recv_ends.borrow().get(&stream_id) else { return Err(StreamError::Done) };
            // Fully drained and ended: the session drops its InStream on
            // seeing this, so the entry can be retired after the poll.
            if stream_id != CTRL_STREAM_ID {
                self.recv_retired.insert(stream_id);
            }
            return Err(match end {
                RecvEnd::Fin => StreamError::Fin,
                RecvEnd::Reset => StreamError::StreamReset(0),
            });
        }
        let n = stream_buf.len().min(buf.len());
        buf[..n].copy_from_slice(&stream_buf[..n]);
        stream_buf.drain(..n);
        Ok(n)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, StreamError> {
        let queued = self.send_bufs.entry(stream_id).or_default();
        queued.0.extend_from_slice(data);
        queued.1 |= fin;
        Ok(data.len())
    }

    fn send_stream_if_capacity(
        &mut self,
        stream_id: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, StreamError> {
        self.send_stream(stream_id, data, fin)
    }

    /// WebTransport's stream-creation API is async (`createUnidirectionalStream()`
    /// returns a `Promise`), but this trait method is sync. So: allocate the id
    /// immediately and return it, and spawn a background task that does the real
    /// creation and registers the writer once it resolves. Any `send_stream` calls
    /// in the meantime just queue in `send_bufs`; `flush_sends` retries until the
    /// writer shows up. Used for the draft 17+ uni-pair control stream (the first
    /// call, from `MoqTransportSession::connect`) — draft < 17 never calls this.
    fn open_uni_stream(&mut self) -> Result<u64, StreamError> {
        let id = self.next_uni_id;
        self.next_uni_id += 4;
        let wt = self.wt.clone();
        let writers = self.writers.clone();
        let wake = self.wake.clone();
        spawn_local(async move {
            let Ok(stream) = JsFuture::from(wt.create_unidirectional_stream()).await else { return };
            let Ok(writable) = stream.dyn_into::<WritableStream>() else { return };
            let Ok(writer) = writable.get_writer() else { return };
            register_writer(&writers, id, writer, &wake);
            wake.notify();
        });
        Ok(id)
    }

    /// Same async-creation strategy as `open_uni_stream`, but also wires up the
    /// readable half via `spawn_reader` since responses arrive on the same stream:
    /// the shared control stream for draft < 17 (the first call, from
    /// `MoqTransportSession::connect`), or a per-request stream for SUBSCRIBE/
    /// PUBLISH/PUBLISH_NAMESPACE on draft 17+.
    fn open_bidi_stream(&mut self) -> Result<u64, StreamError> {
        let id = self.next_bidi_id;
        self.next_bidi_id += 4;
        let wt = self.wt.clone();
        let writers = self.writers.clone();
        let recv_bufs = self.recv_bufs.clone();
        let recv_ends = self.recv_ends.clone();
        let wake = self.wake.clone();
        spawn_local(async move {
            let Ok(bidi) = JsFuture::from(wt.create_bidirectional_stream()).await else { return };
            let Ok(bidi) = bidi.dyn_into::<WebTransportBidirectionalStream>() else { return };
            let Ok(writable) = Reflect::get(bidi.as_ref(), &"writable".into())
                .and_then(|v| v.dyn_into::<WritableStream>())
            else { return };
            let Ok(readable) = Reflect::get(bidi.as_ref(), &"readable".into())
                .and_then(|v| v.dyn_into::<ReadableStream>())
            else { return };
            let Ok(writer) = writable.get_writer() else { return };
            register_writer(&writers, id, writer, &wake);
            wake.notify();
            spawn_stream_reader(recv_bufs, recv_ends, id, readable, wake);
        });
        Ok(id)
    }

    fn set_stream_priority(&mut self, _stream_id: u64, _urgency: u8, _incremental: bool) {}

    /// Aborts the writer, also one still being created. The browser doesn't expose the reset code.
    fn reset_stream(&mut self, stream_id: u64, _code: ResetCode) {
        self.send_bufs.remove(&stream_id);
        let mut writers = self.writers.borrow_mut();
        match writers.open.remove(&stream_id) {
            Some(writer) => { let _ = writer.abort(); }
            None => { writers.reset.insert(stream_id); }
        }
    }

    /// Ignored, the session drains the stream until it ends.
    fn stop_sending(&mut self, _stream_id: u64, _code: ResetCode) -> bool {
        false
    }
    fn stream_stopped(&mut self, stream_id: u64) -> bool {
        self.writers.borrow().stopped.contains(&stream_id)
    }
    /// The browser truncates `reason` to the 1024 bytes WebTransport allows.
    fn close_session(&mut self, code: SessionCloseCode, reason: &str) {
        let info = WebTransportCloseInfo::new();
        info.set_close_code(code.into());
        info.set_reason(reason);
        self.wt.close_with_close_info(&info);
    }

    /// True once for a stream `flush_sends` closed, the session forgets it then.
    fn stream_finished(&self, stream_id: u64) -> bool {
        self.send_finished.borrow_mut().remove(&stream_id)
    }
}

impl AsyncTransport for BrowserWebTransport {
    type DropQueue = moq_core::LocalDropQueue;
    type FlushError = JsValue;

    fn readable_stream_ids(&self) -> Vec<u64> {
        BrowserWebTransport::readable_stream_ids(self)
    }

    async fn flush_sends(&mut self) -> Result<(), JsValue> {
        BrowserWebTransport::flush_sends(self).await
    }

    fn retire_finished_streams(&mut self) {
        BrowserWebTransport::retire_finished_streams(self)
    }

    type ConnectParams = String;
    type ConnectError = JsValue;

    /// Opens the `WebTransport` to `relay_url`, waits for it to be ready,
    /// and starts accepting server-initiated uni streams (SUBSCRIBE_OK
    /// responses on draft 17+'s per-request streams, and track data
    /// streams) — everything a fresh `BrowserWebTransport` needs before
    /// `MoqTransportSession::connect` opens the control stream on top of it.
    async fn connect(relay_url: String) -> Result<(Self, Notify), JsValue> {
        let wt = WebTransport::new(&relay_url)?;
        JsFuture::from(wt.ready()).await?;

        let wake = Notify::new();
        let t = BrowserWebTransport::new(wt.clone(), wake.clone());
        spawn_incoming_stream_acceptor(&wt, t.recv_bufs.clone(), t.recv_ends.clone(), wake.clone())?;
        Ok((t, wake))
    }
}

/// Spawns the task that accepts server-initiated uni streams: draft 17+'s control stream and track data streams.
// TODO server-initiated bidi streams, draft 17+ request streams such as PUBLISH, until then they are never read
fn spawn_incoming_stream_acceptor(
    wt: &WebTransport,
    recv_bufs: SharedRecvBufs,
    recv_ends: SharedRecvEnds,
    wake: Notify,
) -> Result<(), JsValue> {
    let incoming_reader = wt
        .incoming_unidirectional_streams()
        .get_reader()
        .dyn_into::<ReadableStreamDefaultReader>()?;
    // server-originated uni streams: bit 0 = 1 (server), bit 1 = 1 (uni) → 3, 7, 11, ...
    let next_id: Rc<RefCell<u64>> = Rc::new(RefCell::new(3));
    spawn_local(async move {
        loop {
            let res = match JsFuture::from(incoming_reader.read()).await {
                Ok(r) => r,
                Err(_) => break,
            };
            if js_bool(&res, "done") {
                break;
            }
            let stream = match Reflect::get(&res, &"value".into())
                .ok()
                .and_then(|v| v.dyn_into::<ReadableStream>().ok())
            {
                Some(s) => s,
                None => continue,
            };
            let stream_id = {
                let mut id = next_id.borrow_mut();
                let cur = *id;
                *id += 4;
                cur
            };
            recv_bufs.borrow_mut().entry(stream_id).or_default();
            wake.notify();

            spawn_stream_reader(recv_bufs.clone(), recv_ends.clone(), stream_id, stream, wake.clone());
        }
    });
    Ok(())
}
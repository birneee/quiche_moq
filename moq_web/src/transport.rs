//! `moq_core::Transport` implementation bridging the synchronous MoQ session
//! state machine to the browser's async `WebTransport` streams. Reads are
//! pre-buffered by background reader tasks (spawned here and in `lib.rs`)
//! into `recv_bufs`/`recv_fins`; writes are queued in `send_bufs` and
//! flushed with `flush_sends`.

use crate::util::js_bool;
use moq_core::{StreamError, Transport};
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
    WebTransportBidirectionalStream, WritableStream, WritableStreamDefaultWriter,
};

/// The control stream: for draft < 17 this is a single shared bidi stream
/// (the first one `open_bidi_stream` allocates); for draft 17+ it's our half
/// of the uni pair (the first one `open_uni_stream` allocates). Either way
/// it's the first stream `MoqTransportSession::connect` opens.
pub(crate) const CTRL_STREAM_ID: u64 = 0;

pub(crate) type SharedRecvBufs = Rc<RefCell<HashMap<u64, Vec<u8>>>>;
pub(crate) type SharedRecvFins = Rc<RefCell<HashSet<u64>>>;
type SharedWriters = Rc<RefCell<HashMap<u64, WritableStreamDefaultWriter>>>;

/// Scratch buffer size for BYOB reads. Reused across reads on the same
/// stream, so this is a one-time allocation per stream, not per chunk. Sized
/// well above a typical keyframe object so large objects don't need several
/// separate `read()` calls (each a renderer/browser-process IPC round-trip).
const READ_CHUNK_SIZE: u32 = 1024 * 1024;

/// Spawn a background task that reads `stream` to completion, buffering bytes
/// into `recv_bufs[stream_id]` and marking `recv_fins` on EOF, waking the
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
    recv_fins: SharedRecvFins,
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
            let res = match JsFuture::from(reader.read_with_array_buffer_view(&view)).await {
                Ok(r) => r,
                Err(_) => break,
            };
            if js_bool(&res, "done") {
                recv_fins.borrow_mut().insert(stream_id);
                wake.notify();
                break;
            }
            let Some(arr) = Reflect::get(&res, &"value".into())
                .ok()
                .and_then(|v| v.dyn_into::<Uint8Array>().ok())
            else {
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
    pub(crate) recv_fins: SharedRecvFins,
    /// Streams whose FIN has been reported to the session. Their bookkeeping is
    /// dropped in `retire_finished_streams`; without that they would stay in
    /// `recv_bufs` forever and the session would keep re-creating an `InStream`
    /// for each of them on every wake.
    recv_retired: HashSet<u64>,
    send_bufs: HashMap<u64, Vec<u8>>,
    /// Shared with the async tasks spawned by `open_uni_stream`/`open_bidi_stream`,
    /// which register a writer here once the (necessarily async) JS-side stream
    /// creation resolves. Until then, `flush_sends` just leaves queued bytes for
    /// that stream id in `send_bufs` and retries on the next call.
    writers: SharedWriters,
    wt: WebTransport,
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
            recv_fins: Rc::new(RefCell::new(HashSet::new())),
            recv_retired: HashSet::new(),
            send_bufs: HashMap::new(),
            writers: Rc::new(RefCell::new(HashMap::new())),
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
        let mut recv_fins = self.recv_fins.borrow_mut();
        for stream_id in self.recv_retired.drain() {
            recv_bufs.remove(&stream_id);
            recv_fins.remove(&stream_id);
        }
    }

    pub(crate) async fn flush_sends(&mut self) -> Result<(), JsValue> {
        let pending = std::mem::take(&mut self.send_bufs);
        for (stream_id, data) in pending {
            let writer = self.writers.borrow().get(&stream_id).cloned();
            match writer {
                Some(writer) => {
                    let arr = Uint8Array::from(data.as_slice());
                    JsFuture::from(writer.write_with_chunk(&arr)).await?;
                }
                // JS-side stream creation (spawned by open_uni_stream/open_bidi_stream)
                // hasn't resolved yet — requeue for the next flush.
                None => {
                    self.send_bufs.entry(stream_id).or_default().extend(data);
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
            return if self.recv_fins.borrow().contains(&stream_id) {
                // Fully drained and FIN'd: the session drops its InStream on
                // seeing this, so the entry can be retired after the poll.
                if stream_id != CTRL_STREAM_ID {
                    self.recv_retired.insert(stream_id);
                }
                Err(StreamError::Fin)
            } else {
                Err(StreamError::Done)
            };
        }
        let n = stream_buf.len().min(buf.len());
        buf[..n].copy_from_slice(&stream_buf[..n]);
        stream_buf.drain(..n);
        Ok(n)
    }

    fn send_stream(&mut self, stream_id: u64, data: &[u8], _fin: bool) -> Result<usize, StreamError> {
        self.send_bufs.entry(stream_id).or_default().extend_from_slice(data);
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
            writers.borrow_mut().insert(id, writer);
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
        let recv_fins = self.recv_fins.clone();
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
            writers.borrow_mut().insert(id, writer);
            wake.notify();
            spawn_stream_reader(recv_bufs, recv_fins, id, readable, wake);
        });
        Ok(id)
    }

    fn set_stream_priority(&mut self, _stream_id: u64, _urgency: u8, _incremental: bool) {}
    fn reset_stream(&mut self, _stream_id: u64, _error_code: u64) {}
    fn stop_sending(&mut self, _stream_id: u64, _error_code: u64) {}
    fn close_session(&mut self, _error_code: u64, _reason: &str) {}
}

impl AsyncTransport for BrowserWebTransport {
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
        spawn_incoming_stream_acceptor(&wt, t.recv_bufs.clone(), t.recv_fins.clone(), wake.clone())?;
        Ok((t, wake))
    }
}

/// Spawns the task that accepts server-initiated uni streams (SUBSCRIBE_OK
/// responses on draft 17+'s per-request streams, and track data streams).
fn spawn_incoming_stream_acceptor(
    wt: &WebTransport,
    recv_bufs: SharedRecvBufs,
    recv_fins: SharedRecvFins,
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

            spawn_stream_reader(recv_bufs.clone(), recv_fins.clone(), stream_id, stream, wake.clone());
        }
    });
    Ok(())
}
//! `MoqSession`/`MoqTrack`: the public JS-facing API. `MoqSession::connect`
//! does the WebTransport + MoQ handshake and starts a background receive
//! loop (both delegated to `moq_core_async::MoqSession`, the same type
//! `moq_tokio` uses); `MoqTrack::render_video`/`render_audio` each spawn a
//! task that consumes `next_object()` and decodes it — thin, decode-specific
//! sugar over the raw object stream, not part of the receive loop itself.

use crate::decode::{decode_object, Decoders};
use crate::transport::BrowserWebTransport;
use moq_core_async::AsyncTransport;
use log::info;
use moq_core::wire::KeyValuePair;
use moq_core::{Config, LocalDropQueue, Subscription};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{js_sys, HtmlCanvasElement, WebTransport, WebTransportStats};

#[wasm_bindgen]
pub struct MoqSession {
    session: moq_core_async::MoqSession<BrowserWebTransport>,
    /// For `transport_stats`, the session actor owns the `BrowserWebTransport` wrapping it.
    wt: WebTransport,
}

#[wasm_bindgen]
impl MoqSession {
    /// Connects to `relay_url` and starts the background receive loop.
    /// `draft` pins the negotiated MoQ version ("18", "draft18", or "" for
    /// the library default — auto-negotiates the highest mutually supported
    /// version); see `moq_cli`'s `--setup-version` for the same knob outside
    /// the browser.
    pub async fn connect(relay_url: String, draft: String) -> Result<MoqSession, JsValue> {
        console_log::init_with_level(log::Level::Info).unwrap_or(());
        console_error_panic_hook::set_once();

        let config = build_config(&draft)?;
        let (t, wake) = BrowserWebTransport::connect(relay_url).await?;
        let wt = t.wt.clone();
        let (session, receive_loop) = moq_core_async::MoqSession::connect_transport(t, wake, config)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        info!("negotiated MoQ session established");
        spawn_local(receive_loop);

        Ok(MoqSession { session, wt })
    }

    /// The underlying QUIC connection, i.e. to the next hop (the relay or server this session connected to), from
    /// `WebTransport.getStats()`: its fields, RTTs in ms, each present only if the browser reports it. An empty
    /// object if `getStats()` isn't supported.
    pub async fn transport_stats(&self) -> Result<JsValue, JsValue> {
        let wt = &self.wt;
        if !js_sys::Reflect::has(wt, &JsValue::from_str("getStats"))? {
            return js_object([]);
        }
        let stats: WebTransportStats = JsFuture::from(wt.get_stats()).await?.unchecked_into();
        let datagrams = stats.get_datagrams();
        let count = |n: Option<u32>| n.map(f64::from);
        js_object([
            ("bytesSent", stats.get_bytes_sent()),
            ("bytesReceived", stats.get_bytes_received()),
            ("packetsSent", stats.get_packets_sent()),
            ("packetsReceived", stats.get_packets_received()),
            ("packetsLost", stats.get_packets_lost()),
            ("smoothedRtt", stats.get_smoothed_rtt()),
            ("rttVariation", stats.get_rtt_variation()),
            ("minRtt", stats.get_min_rtt()),
            ("numIncomingStreamsCreated", count(stats.get_num_incoming_streams_created())),
            ("numOutgoingStreamsCreated", count(stats.get_num_outgoing_streams_created())),
            ("datagramsDroppedIncoming", datagrams.as_ref().and_then(|d| d.get_dropped_incoming())),
            ("datagramsExpiredOutgoing", datagrams.as_ref().and_then(|d| d.get_expired_outgoing())),
            ("datagramsLostOutgoing", datagrams.as_ref().and_then(|d| d.get_lost_outgoing())),
        ])
    }

    /// What the MoQ session has open, as of its last receive cycle: `{receivedTracks, sentTracks, dataStreams}`.
    pub fn session_stats(&self) -> Result<JsValue, JsValue> {
        let s = self.session.stats();
        js_object([
            ("receivedTracks", Some(s.received_tracks as f64)),
            ("sentTracks", Some(s.sent_tracks as f64)),
            ("dataStreams", Some(s.data_streams as f64)),
        ])
    }

    /// Subscribes to `namespace_trackname` (e.g. `"tos--video"`, matching
    /// `moq_cli`'s `namespace--track` convention) and resolves once the
    /// peer accepts or rejects it. Safe to call multiple times concurrently
    /// (e.g. `Promise.all([session.subscribe(a), session.subscribe(b)])`) —
    /// each call is independent and doesn't wait for any other's round trip.
    pub async fn subscribe(&self, namespace_trackname: String) -> Result<MoqTrack, JsValue> {
        info!("sending subscribe for {namespace_trackname:?}");
        let track = self
            .session
            .subscribe(&namespace_trackname)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        info!("subscribe ok, request_id={}", track.subscription.request_id());
        Ok(MoqTrack {
            subscription: RefCell::new(Some(track.subscription)),
            stats: track.objects.clone(),
            objects: RefCell::new(Some(track.objects)),
            decoders: Rc::new(Decoders::new()),
            on_object: Rc::new(RefCell::new(None)),
        })
    }
}

#[wasm_bindgen]
pub struct MoqTrack {
    /// Taken by `unsubscribe`.
    subscription: RefCell<Option<Subscription<LocalDropQueue>>>,
    /// Taken by the first `render_video`/`render_audio`.
    objects: RefCell<Option<moq_core_async::TrackObjects>>,
    /// The same buffer as `objects`, for `stats`.
    stats: moq_core_async::TrackObjects,
    decoders: Rc<Decoders>,
    on_object: Rc<RefCell<Option<js_sys::Function>>>,
}

#[wasm_bindgen]
impl MoqTrack {
    /// Decodes this track as video and renders it to `canvas`. Throws if
    /// the track is already rendered.
    pub fn render_video(&self, canvas: HtmlCanvasElement) -> Result<(), JsError> {
        self.decoders.set_video_canvas(canvas);
        self.spawn_decode_loop(true)
    }

    /// Decodes this track as audio and plays it back (muted until the
    /// module-level `unmute()` is called, per browser autoplay policy).
    /// Throws if the track is already rendered.
    pub fn render_audio(&self) -> Result<(), JsError> {
        self.spawn_decode_loop(false)
    }

    /// Calls `callback` with each object `render_video`/`render_audio`
    /// receive, just before decoding it: `{group, object, size, headers}`.
    /// - `size`: payload bytes
    /// - `headers`: every object extension header (LOC properties and any
    ///   others), keyed by type ID in decimal — a Number for varint types
    ///   (even IDs; exact up to 2^53), a Uint8Array for byte-string types (odd)
    /// - replaces any earlier callback; can be set before or after rendering starts
    pub fn on_object(&self, callback: js_sys::Function) {
        *self.on_object.borrow_mut() = Some(callback);
    }

    pub fn unsubscribe(&self) {
        drop(self.subscription.borrow_mut().take());
    }

    /// The track's playout buffer, see `moq_core_async::TrackStats`:
    /// - cumulative: `objects`, `bytes` played, `discarded` (no room), `skipped` (reset, evicted or late)
    /// - now: `cached`, `progressing` (still arriving), `ready` (buffered ahead of playout), `groupsBehind`
    ///   (between the newest cached object and playout)
    pub fn stats(&self) -> Result<JsValue, JsValue> {
        let s = self.stats.stats();
        let group = |location: Option<moq_core::wire::Location>| location.map_or(0, |l| l.group);
        js_object([
            ("objects", Some(s.objects as f64)),
            ("bytes", Some(s.bytes as f64)),
            ("discarded", Some(s.discarded as f64)),
            ("skipped", Some(s.skipped as f64)),
            ("cached", Some(s.cached as f64)),
            ("progressing", Some(s.progressing as f64)),
            ("ready", Some(s.ready as f64)),
            ("groupsBehind", Some(group(s.newest).saturating_sub(group(s.played)) as f64)),
        ])
    }
}

/// A JS object of the fields with a value.
fn js_object<'a>(fields: impl IntoIterator<Item = (&'a str, Option<f64>)>) -> Result<JsValue, JsValue> {
    let obj = js_sys::Object::new();
    for (key, value) in fields {
        if let Some(value) = value {
            js_sys::Reflect::set(&obj, &JsValue::from_str(key), &JsValue::from_f64(value))?;
        }
    }
    Ok(obj.into())
}

impl MoqTrack {
    /// Decodes each of the track's objects.
    fn spawn_decode_loop(&self, is_video: bool) -> Result<(), JsError> {
        let mut objects = self.objects.borrow_mut().take().ok_or_else(|| JsError::new("track is already rendered"))?;
        let (decoders, on_object) = (self.decoders.clone(), self.on_object.clone());
        spawn_local(async move {
            // timescale defaults to microseconds until a LoC config header says otherwise.
            let mut timescale: u64 = 1_000_000;
            loop {
                let obj = objects.next_object().await;
                match obj {
                    Some(obj) => {
                        // cloned: the callback may replace itself via `on_object`
                        let callback = on_object.borrow().clone();
                        if let Some(callback) = callback
                            && let Err(e) = callback.call1(&JsValue::NULL, &object_info(&obj)) {
                                log::error!("on_object callback threw: {e:?}");
                            }
                        decode_object(&obj.payload, &obj.properties, is_video, &mut timescale, &decoders)
                    }
                    None => return,
                }
            }
        });
        Ok(())
    }
}

/// The `{group, object, size, headers}` value `MoqTrack::on_object` passes.
fn object_info(obj: &moq_core_async::CompleteObject) -> JsValue {
    let set = |target: &js_sys::Object, key: &str, value: JsValue| {
        js_sys::Reflect::set(target, &JsValue::from_str(key), &value).unwrap();
    };
    let headers = js_sys::Object::new();
    for kvp in obj.properties.iter() {
        let value = match kvp {
            KeyValuePair::Varint(_, v) => JsValue::from_f64(*v as f64),
            KeyValuePair::Bytes(_, b) => js_sys::Uint8Array::from(b.as_slice()).into(),
        };
        set(&headers, &kvp.ty().to_string(), value);
    }
    let info = js_sys::Object::new();
    set(&info, "group", JsValue::from_f64(obj.location.group as f64));
    set(&info, "object", JsValue::from_f64(obj.location.object as f64));
    set(&info, "size", JsValue::from_f64(obj.payload.len() as f64));
    set(&info, "headers", headers.into());
    info.into()
}

fn build_config(draft: &str) -> Result<Config, JsValue> {
    let mut config = Config::default();
    if draft.is_empty() {
        info!("no MoQ draft pinned; offering {:?}", config.version_negotiation);
        return Ok(config);
    }
    let v: moq_core::wire::Version = draft.parse().map_err(|e: String| JsValue::from_str(&e))?;
    config.version_negotiation = moq_core::VersionNegotiation::only(v);
    info!("pinned to MoQ draft {draft} (0x{:x})", v.to_wire());
    Ok(config)
}

//! `MoqSession`/`MoqTrack`: the public JS-facing API. `MoqSession::connect`
//! does the WebTransport + MoQ handshake and starts a background receive
//! loop (both delegated to `moq_core_async::MoqSession`, the same type
//! `moq_tokio` uses); `MoqTrack::render_video`/`render_audio` each spawn a
//! task that consumes `next_object()` and decodes it — thin, decode-specific
//! sugar over the raw object stream, not part of the receive loop itself.

use crate::decode::{decode_object, Decoders};
use crate::transport::BrowserWebTransport;
use log::info;
use moq_core::wire::RequestId;
use moq_core::Config;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;

#[wasm_bindgen]
pub struct MoqSession(moq_core_async::MoqSession<BrowserWebTransport>);

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
        let (session, receive_loop) = moq_core_async::MoqSession::connect(relay_url, config)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        info!("negotiated MoQ session established");
        spawn_local(receive_loop);

        Ok(MoqSession(session))
    }

    /// Subscribes to `namespace_trackname` (e.g. `"tos--video"`, matching
    /// `moq_cli`'s `namespace--track` convention) and resolves once the
    /// peer accepts or rejects it. Safe to call multiple times concurrently
    /// (e.g. `Promise.all([session.subscribe(a), session.subscribe(b)])`) —
    /// each call is independent and doesn't wait for any other's round trip.
    pub async fn subscribe(&self, namespace_trackname: String) -> Result<MoqTrack, JsValue> {
        info!("sending subscribe for {namespace_trackname:?}");
        let track = self
            .0
            .subscribe(&namespace_trackname)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        info!("subscribe ok, request_id={}", track.request_id);
        Ok(MoqTrack {
            session: self.0.clone(),
            request_id: track.request_id,
            track: Rc::new(RefCell::new(track)),
            decoders: Rc::new(Decoders::new()),
        })
    }
}

#[wasm_bindgen]
pub struct MoqTrack {
    session: moq_core_async::MoqSession<BrowserWebTransport>,
    request_id: RequestId,
    track: Rc<RefCell<moq_core_async::MoqTrack>>,
    decoders: Rc<Decoders>,
}

#[wasm_bindgen]
impl MoqTrack {
    /// Decodes this track as video and renders it to `canvas`.
    pub fn render_video(&self, canvas: HtmlCanvasElement) {
        self.decoders.set_video_canvas(canvas);
        spawn_decode_loop(self.track.clone(), self.decoders.clone(), true);
    }

    /// Decodes this track as audio and plays it back (muted until the
    /// module-level `unmute()` is called, per browser autoplay policy).
    pub fn render_audio(&self) {
        spawn_decode_loop(self.track.clone(), self.decoders.clone(), false);
    }

    pub fn unsubscribe(&self) {
        self.session.unsubscribe(self.request_id);
    }
}

/// Consumes `track.next_object()` in a loop and decodes each one — the sugar
/// `render_video`/`render_audio` provide on top of the raw object stream
/// `moq_core_async::MoqTrack` gives every platform.
fn spawn_decode_loop(track: Rc<RefCell<moq_core_async::MoqTrack>>, decoders: Rc<Decoders>, is_video: bool) {
    spawn_local(async move {
        // timescale defaults to microseconds until a LoC config header says otherwise.
        let mut timescale: u64 = 1_000_000;
        loop {
            let obj = track.borrow_mut().next_object().await;
            match obj {
                Some(obj) => decode_object(&obj.payload, &obj.ext_hdrs, is_video, &mut timescale, &decoders),
                None => return,
            }
        }
    });
}

fn build_config(draft: &str) -> Result<Config, JsValue> {
    let mut config = Config::default();
    if draft.is_empty() {
        info!("no MoQ draft pinned; offering {:?}", config.version_negotiation);
        return Ok(config);
    }
    let v = crate::parse_draft_version(draft).map_err(|e| JsValue::from_str(&e))?;
    // draft >= 15 negotiates via ALPN/WT-Available-Protocols, not
    // CLIENT_SETUP's version list — pinning to one of those versions means
    // there's nothing left to offer in-band.
    config.version_negotiation = if v >= moq_core::wire::MOQ_VERSION_DRAFT_15 {
        moq_core::VersionNegotiation::Pinned(v)
    } else {
        moq_core::VersionNegotiation::InBand { candidates: vec![v] }
    };
    info!("pinned to MoQ draft {draft} (0x{v:x})");
    Ok(config)
}

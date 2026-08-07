//! Dispatches a completed MoQ object to the right decoder (`VideoDecoder` or
//! `AudioDecoder`), configuring it from LoC codec-config headers on first use.

use crate::audio::{encoded_audio_chunk_init, init_audio_decoder, register_audio_play, AudioPlaybackState};
use crate::video::{encoded_video_chunk_init, init_video_decoder};
use moq_core::wire::KeyValuePairs;
use moq_loc::{LocProperties, Timescale, Timestamp};
use std::cell::RefCell;
use std::rc::Rc;
use web_sys::{
    AudioDecoder, EncodedAudioChunk, EncodedAudioChunkType, EncodedVideoChunk,
    EncodedVideoChunkType, HtmlCanvasElement, VideoDecoder,
};

/// Decoder-related state for one `MoqTrack`'s decode loop, constructed once
/// per `subscribe()` call and shared via its own `Rc` — no need for each
/// field to carry its own. `canvas` starts unset; `MoqTrack::render_video`
/// fills it in once a caller wants the track decoded to a canvas, always
/// before any object needing it can arrive (subscribing and calling
/// `render_video` both happen before any data flows for a fresh track).
pub(crate) struct Decoders {
    v_dec: RefCell<Option<VideoDecoder>>,
    a_dec: RefCell<Option<AudioDecoder>>,
    audio_play: Rc<RefCell<Option<AudioPlaybackState>>>,
    canvas: RefCell<Option<HtmlCanvasElement>>,
}

impl Decoders {
    pub(crate) fn new() -> Self {
        let audio_play = Rc::new(RefCell::new(None));
        register_audio_play(audio_play.clone());
        Self {
            v_dec: RefCell::new(None),
            a_dec: RefCell::new(None),
            audio_play,
            canvas: RefCell::new(None),
        }
    }

    pub(crate) fn set_video_canvas(&self, canvas: HtmlCanvasElement) {
        *self.canvas.borrow_mut() = Some(canvas);
    }
}

pub(crate) fn decode_object(
    payload: &[u8],
    ext_hdrs: &KeyValuePairs,
    is_video: bool,
    timescale: &mut u64,
    decoders: &Decoders,
) {
    let loc = LocProperties::from_kvps(ext_hdrs);
    if let Some(Timescale(ts)) = loc.timescale() {
        *timescale = ts;
    }
    let ts = *timescale;
    let timestamp_us = loc
        .timestamp()
        .map(|Timestamp(t)| (t as f64 * 1_000_000.0 / ts as f64) as i64)
        .unwrap_or(0);

    if is_video {
        if let Some(cfg) = loc.video_config() {
            if decoders.v_dec.borrow().is_none() {
                match decoders.canvas.borrow().clone() {
                    Some(canvas) => match init_video_decoder(&cfg.0, canvas) {
                        Ok(dec) => *decoders.v_dec.borrow_mut() = Some(dec),
                        Err(e) => log::warn!("video decoder init failed: {e}"),
                    },
                    None => log::warn!("video config arrived before render_video() was called"),
                }
            }
        }
    } else if let Some(cfg) = loc.audio_config() {
        if decoders.a_dec.borrow().is_none() {
            match init_audio_decoder(&cfg.0, decoders.audio_play.clone()) {
                Ok(dec) => *decoders.a_dec.borrow_mut() = Some(dec),
                Err(e) => log::warn!("audio decoder init failed: {e}"),
            }
        }
    }

    let is_keyframe = loc.video_config().is_some()
        || loc.video_frame_marking().is_some_and(|m| m.is_independent());

    feed_decoder(payload, is_keyframe, is_video, timestamp_us, decoders);
}

fn feed_decoder(
    payload: &[u8],
    is_keyframe: bool,
    is_video: bool,
    timestamp_us: i64,
    decoders: &Decoders,
) {
    if is_video {
        let dec_ref = decoders.v_dec.borrow();
        if let Some(ref dec) = *dec_ref {
            let ty = if is_keyframe {
                EncodedVideoChunkType::Key
            } else {
                EncodedVideoChunkType::Delta
            };
            let mut buf = payload.to_vec();
            let init = encoded_video_chunk_init(&mut buf, timestamp_us as f64, ty);
            if let Ok(chunk) = EncodedVideoChunk::new(&init) {
                if let Err(e) = VideoDecoder::decode(dec, &chunk) {
                    log::warn!("VideoDecoder::decode: {e:?}");
                }
            }
        }
    } else {
        let dec_ref = decoders.a_dec.borrow();
        if let Some(ref dec) = *dec_ref {
            let mut buf = payload.to_vec();
            let init = encoded_audio_chunk_init(&mut buf, timestamp_us as f64, EncodedAudioChunkType::Key);
            if let Ok(chunk) = EncodedAudioChunk::new(&init) {
                if let Err(e) = AudioDecoder::decode(dec, &chunk) {
                    log::warn!("AudioDecoder::decode: {e:?}");
                }
            }
        }
    }
}
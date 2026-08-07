//! `AudioDecoder` setup from LoC `mp4a` codec config, decoded-audio playback
//! scheduling via `AudioContext`, and the `EncodedAudioChunk` construction
//! helper used to feed the decoder.

use crate::mp4::{find_asc_in_descriptors, find_box_anywhere};
use log::info;
use moq_pacing::{Band, PacingThresholds};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::js_sys::{self, Float32Array, Reflect};
use web_sys::{
    AudioBuffer, AudioContext, AudioData, AudioDataCopyToOptions, AudioDecoder,
    AudioDecoderConfig, AudioDecoderInit, AudioDestinationNode, EncodedAudioChunkInit,
    EncodedAudioChunkType,
};

// Browsers block audible autoplay without a user gesture. Rather than fight
// that (e.g. by muting via gain while still scheduling playback against a
// clock that may be frozen until resumed, which just builds up a backlog
// that all plays back at once on unmute), audio is simply not scheduled at
// all until the page's "Unmute" button has been clicked.
thread_local! {
    static UNMUTED: Cell<bool> = const { Cell::new(false) };
    static AUDIO_PLAY: RefCell<Option<Rc<RefCell<Option<AudioPlaybackState>>>>> =
        const { RefCell::new(None) };
}

pub(crate) fn register_audio_play(audio_play: Rc<RefCell<Option<AudioPlaybackState>>>) {
    AUDIO_PLAY.with(|cell| *cell.borrow_mut() = Some(audio_play));
}

/// Called from the page's "Unmute" button.
#[wasm_bindgen]
pub fn unmute() {
    UNMUTED.with(|c| c.set(true));
    AUDIO_PLAY.with(|cell| {
        let Some(audio_play) = cell.borrow().clone() else { return };
        if let Some(state) = audio_play.borrow_mut().as_mut() {
            // Restart the pacing baseline from now, instead of from whenever
            // the (possibly long-suspended) context was first created.
            state.prev_ts_us = None;
            state.base_play_time = state.ctx.current_time() + 0.2;
            state.output_elapsed_s = 0.0;
            let _ = state.ctx.resume();
        }
    });
}

// ── audio playback state (shared across subgroup tasks) ─────────────────────

pub(crate) struct AudioPlaybackState {
    ctx: AudioContext,
    /// `ctx.destination()` fetched once — it's a fixed property of the
    /// context for its whole lifetime, so re-fetching it on every chunk (as
    /// before) was pure per-chunk overhead with no effect.
    dest: AudioDestinationNode,
    prev_ts_us: Option<i64>,
    base_play_time: f64,
    /// Accumulated, pacer-scaled offset from `base_play_time`. Advanced by
    /// `moq_pacing`-scaled inter-chunk deltas rather than the raw source
    /// timestamp delta, so the presentation clock can be nudged to drain or
    /// rebuild buffer margin (see `AUDIO_PACING`).
    output_elapsed_s: f64,
}

/// Speed curve for audio presentation pacing. Buffer level is milliseconds
/// of lead: how far ahead of `ctx.current_time()` the presentation timeline
/// (`base_play_time + output_elapsed_s`) currently sits. `base_play_time`
/// starts 200ms ahead (see `init_audio_decoder`), so these bands are
/// centered around that target lead.
const AUDIO_PACING: PacingThresholds<'static> = PacingThresholds {
    bands: &[
        Band { above: /* 400ms lead */ 400.0, speed: 0.90 },
        Band { above: /* 250ms lead */ 250.0, speed: 0.95 },
        Band { above: /* 0ms lead   */ 0.0, speed: 1.0 },
        Band { above: /* starved/behind */ f32::NEG_INFINITY, speed: 1.05 },
    ],
};

pub(crate) fn init_audio_decoder(
    codec_config: &[u8],
    audio_play: Rc<RefCell<Option<AudioPlaybackState>>>,
) -> Result<AudioDecoder, String> {
    if codec_config.len() < 4 {
        return Err("codec_config too short".into());
    }
    let fourcc = &codec_config[..4];
    let stsd_body = &codec_config[4..];

    let (codec_str, sample_rate, channels, mut asc) = match fourcc {
        b"mp4a" => {
            if stsd_body.len() < 28 {
                return Err("mp4a stsd body too short".into());
            }
            let channels = u16::from_be_bytes(stsd_body[16..18].try_into().unwrap());
            let sample_rate =
                u32::from_be_bytes(stsd_body[24..28].try_into().unwrap()) >> 16;
            let esds = find_box_anywhere(stsd_body, b"esds")
                .ok_or("no esds box")?;
            let asc = if esds.len() > 4 {
                find_asc_in_descriptors(&esds[4..]).unwrap_or_default()
            } else {
                vec![]
            };
            ("mp4a.40.2".to_string(), sample_rate, channels, asc)
        }
        other => {
            return Err(format!(
                "unsupported audio fourcc: {}",
                String::from_utf8_lossy(other)
            ))
        }
    };

    let ctx = AudioContext::new().map_err(|e| format!("AudioContext::new: {e:?}"))?;
    let base_play_time = ctx.current_time() + 0.2;
    let dest = ctx.destination();
    *audio_play.borrow_mut() = Some(AudioPlaybackState {
        ctx,
        dest,
        prev_ts_us: None,
        base_play_time,
        output_elapsed_s: 0.0,
    });

    let err_cb = Closure::wrap(Box::new(|e: JsValue| {
        log::error!("AudioDecoder error: {e:?}");
    }) as Box<dyn FnMut(JsValue)>);

    let play_ref = audio_play.clone();
    let output_cb = Closure::wrap(Box::new(move |data: JsValue| {
        let Ok(audio_data) = data.dyn_into::<AudioData>() else {
            return;
        };
        if let Err(e) = play_audio_data(&audio_data, &play_ref) {
            log::warn!("audio playback error: {e:?}");
        }
        audio_data.close();
    }) as Box<dyn FnMut(JsValue)>);

    let init = AudioDecoderInit::new(
        err_cb.as_ref().unchecked_ref(),
        output_cb.as_ref().unchecked_ref(),
    );
    err_cb.forget();
    output_cb.forget();

    let dec = AudioDecoder::new(&init).map_err(|e| format!("AudioDecoder::new: {e:?}"))?;
    let cfg = AudioDecoderConfig::new(&codec_str, channels as u32, sample_rate);
    if !asc.is_empty() {
        AudioDecoderConfig::set_description_u8_slice(&cfg, &mut asc);
    }
    AudioDecoder::configure(&dec, &cfg)
        .map_err(|e| format!("AudioDecoder::configure: {e:?}"))?;

    info!("AudioDecoder configured: {codec_str} {channels}ch {sample_rate}Hz");
    Ok(dec)
}

// ── audio playback ───────────────────────────────────────────────────────────

fn play_audio_data(
    audio_data: &AudioData,
    play: &Rc<RefCell<Option<AudioPlaybackState>>>,
) -> Result<(), JsValue> {
    if !UNMUTED.with(|c| c.get()) {
        return Ok(());
    }
    let mut state_opt = play.borrow_mut();
    let state = match state_opt.as_mut() {
        Some(s) => s,
        None => return Ok(()),
    };

    let ts_us = audio_data.timestamp() as i64;
    let nominal_dt_s = match state.prev_ts_us {
        Some(prev) => (ts_us - prev) as f64 / 1_000_000.0,
        None => 0.0,
    };
    state.prev_ts_us = Some(ts_us);

    let lead_ms = ((state.base_play_time + state.output_elapsed_s) - state.ctx.current_time()) * 1000.0;
    state.output_elapsed_s += AUDIO_PACING.scale(nominal_dt_s as f32, lead_ms as f32) as f64;
    let play_at = (state.base_play_time + state.output_elapsed_s).max(state.ctx.current_time());

    let n_ch = audio_data.number_of_channels();
    let n_frames = audio_data.number_of_frames();
    let sample_rate = audio_data.sample_rate();

    let audio_buf = state.ctx.create_buffer(n_ch, n_frames, sample_rate)?;

    for ch in 0..n_ch {
        let float_arr = Float32Array::new_with_length(n_frames);
        let copy_opts = AudioDataCopyToOptions::new(ch);
        Reflect::set(
            &copy_opts,
            &"format".into(),
            &JsValue::from_str("f32-planar"),
        )?;
        AudioData::copy_to_with_buffer_source(
            audio_data,
            float_arr.unchecked_ref::<js_sys::Object>(),
            &copy_opts,
        )?;
        AudioBuffer::copy_to_channel_with_f32_array(&audio_buf, &float_arr, ch as i32)?;
    }

    let src = state.ctx.create_buffer_source()?;
    src.set_buffer(Some(&audio_buf));

    src.connect_with_audio_node(&state.dest)?;

    src.start_with_when(play_at)?;
    Ok(())
}

// ── EncodedChunk init helper ─────────────────────────────────────────────────

pub(crate) fn encoded_audio_chunk_init(
    payload: &mut [u8],
    ts: f64,
    ty: EncodedAudioChunkType,
) -> EncodedAudioChunkInit {
    let init =
        js_sys::Object::new().unchecked_into::<EncodedAudioChunkInit>();
    EncodedAudioChunkInit::set_data_u8_slice(&init, payload);
    EncodedAudioChunkInit::set_timestamp_f64(&init, ts);
    EncodedAudioChunkInit::set_type(&init, ty);
    init
}
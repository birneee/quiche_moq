//! `VideoDecoder` setup from LoC `avc1`/`avc3` codec config, and the
//! `EncodedVideoChunk` construction helper used to feed it.

use crate::mp4::find_box_anywhere;
use log::info;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::js_sys;
use web_sys::{
    CanvasRenderingContext2d, EncodedVideoChunkInit, EncodedVideoChunkType, HtmlCanvasElement,
    VideoDecoder, VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

pub(crate) fn init_video_decoder(
    codec_config: &[u8],
    canvas: HtmlCanvasElement,
) -> Result<VideoDecoder, String> {
    if codec_config.len() < 4 {
        return Err("codec_config too short".into());
    }
    let fourcc = &codec_config[..4];
    let stsd_body = &codec_config[4..];

    let (codec_str, mut description) = match fourcc {
        b"avc1" | b"avc3" => {
            let avcc = find_box_anywhere(stsd_body, b"avcC")
                .ok_or("no avcC box")?
                .to_vec();
            if avcc.len() < 4 {
                return Err("avcC too short".into());
            }
            let codec_str =
                format!("avc1.{:02X}{:02X}{:02X}", avcc[1], avcc[2], avcc[3]);
            (codec_str, avcc)
        }
        other => {
            return Err(format!(
                "unsupported video fourcc: {}",
                String::from_utf8_lossy(other)
            ))
        }
    };

    // Fetched once and reused for every frame: a canvas resize clears/resizes its
    // backing bitmap but doesn't invalidate the context handle, so re-fetching it
    // every frame (as well as re-setting width/height every frame even when
    // unchanged, below) was pure per-frame overhead with no effect.
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(|e| format!("canvas.get_context: {e:?}"))?
        .ok_or("canvas has no 2d context")?
        .dyn_into()
        .map_err(|_| "canvas 2d context has unexpected type".to_string())?;

    let err_cb = Closure::wrap(Box::new(|e: JsValue| {
        log::error!("VideoDecoder error: {e:?}");
    }) as Box<dyn FnMut(JsValue)>);

    let mut canvas_size: Option<(u32, u32)> = None;
    let output_cb = Closure::wrap(Box::new(move |frame: JsValue| {
        let Ok(frame) = frame.dyn_into::<VideoFrame>() else {
            return;
        };
        let w = frame.display_width();
        let h = frame.display_height();
        if canvas_size != Some((w, h)) {
            canvas.set_width(w);
            canvas.set_height(h);
            canvas_size = Some((w, h));
        }
        let _ = ctx.draw_image_with_video_frame(&frame, 0.0, 0.0);
        frame.close();
    }) as Box<dyn FnMut(JsValue)>);

    let init = VideoDecoderInit::new(
        err_cb.as_ref().unchecked_ref(),
        output_cb.as_ref().unchecked_ref(),
    );
    err_cb.forget();
    output_cb.forget();

    let dec = VideoDecoder::new(&init).map_err(|e| format!("VideoDecoder::new: {e:?}"))?;
    let cfg = VideoDecoderConfig::new(&codec_str);
    VideoDecoderConfig::set_description_u8_slice(&cfg, &mut description);
    VideoDecoder::configure(&dec, &cfg)
        .map_err(|e| format!("VideoDecoder::configure: {e:?}"))?;

    info!("VideoDecoder configured: {codec_str}");
    Ok(dec)
}

pub(crate) fn encoded_video_chunk_init(
    payload: &mut [u8],
    ts: f64,
    ty: EncodedVideoChunkType,
) -> EncodedVideoChunkInit {
    let init =
        js_sys::Object::new().unchecked_into::<EncodedVideoChunkInit>();
    EncodedVideoChunkInit::set_data_u8_slice(&init, payload);
    EncodedVideoChunkInit::set_timestamp_f64(&init, ts);
    EncodedVideoChunkInit::set_type(&init, ty);
    init
}
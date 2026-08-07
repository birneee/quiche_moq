//! Writes received LoC objects to a file or stdout.
//!
//! Objects flow through the shared `LocalTrack::cache` (a `PackedObjectCache`).
//! Each `FileOutputState` keeps a `last_written` cursor into that cache.  On
//! every drain call it pulls the next complete object, encodes it (fMP4 fragment,
//! raw bytes, or line-delimited text), and writes the result non-blocking.  If
//! the fd blocks, encoded bytes are held in `buf` until the next WRITABLE event.
//! If the upstream cache evicts an object before this output has written it, the
//! object is silently skipped — same semantics as the object-cache → subscriber
//! relay path.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;

use moq_loc::{LocProperties, Timescale, Timestamp};
use moq_object_cache::{CachedObjectRef, ObjectCache, PackedObjectCache};
use crate::fmp4_write;
use moq_wire::Location;

/// Output format for a subscribed track. Used as both the initial constructor
/// argument and the running state — `Auto` resolves to `FMp4` or `Lines` on
/// the first received object; the other variants are set immediately.
#[derive(Debug, Clone)]
pub enum OutputKind {
    /// Auto-detect from LoC extension headers on the first object.
    /// Resolves to `FMp4` if VideoConfig is present, `Lines` otherwise.
    Auto { delimiter: u8 },
    /// Raw concatenated payloads — no framing, no delimiter.
    Raw,
    /// Reconstruct an fMP4 container (ftyp + moov + mdat) from LoC headers.
    FMp4 {
        timescale: u32,
        seq: u32,
        prev_decode_time: Option<u64>,
        output_decode_time: u64,
        moov_written: bool,
    },
    /// Append `delimiter` after each object payload.
    Lines { delimiter: u8 },
}

impl OutputKind {
    pub fn auto(delimiter: u8) -> Self { Self::Auto { delimiter } }
    pub fn raw() -> Self { Self::Raw }
    pub fn fmp4() -> Self {
        Self::FMp4 {
            timescale: 1_000_000, seq: 0, prev_decode_time: None,
            output_decode_time: 0, moov_written: false,
        }
    }
    pub fn lines(delimiter: u8) -> Self { Self::Lines { delimiter } }
}

pub struct FileOutputState {
    file: File,
    kind: OutputKind,
    /// Cursor: location of the last object successfully written to `file`.
    last_written: Option<Location>,
    /// Encoded bytes not yet flushed (non-blocking write left them behind).
    buf: VecDeque<Vec<u8>>,
    partial: usize,
}

impl FileOutputState {
    pub fn new(file: File, kind: OutputKind) -> Self {
        Self { file, kind, last_written: None, buf: VecDeque::new(), partial: 0 }
    }

    /// Pull the next object(s) from `cache`, encode, and write non-blocking.
    /// Stops when the fd blocks (remainder stays in `buf`) or the cache is
    /// exhausted.  Call on every WRITABLE event and after every cache push.
    pub fn drain(&mut self, cache: &PackedObjectCache) {
        drain_pending(&self.file, &mut self.buf, &mut self.partial);
        if !self.buf.is_empty() { return; }

        loop {
            let obj = match cache.next_after(self.last_written) {
                Some(o) if o.is_complete() => o,
                _ => break,
            };
            let loc = obj.location();
            self.encode(obj);
            self.last_written = Some(loc);

            drain_pending(&self.file, &mut self.buf, &mut self.partial);
            if !self.buf.is_empty() { return; }
        }
    }

    fn encode(&mut self, obj: CachedObjectRef<'_>) {
        let payload = obj.readable_from(0);
        let props = LocProperties::from_kvps(obj.ext_hdrs());

        if let OutputKind::Auto { delimiter } = self.kind {
            self.kind = if props.codec_config().is_some() {
                OutputKind::fmp4()
            } else {
                OutputKind::Lines { delimiter }
            };
        }

        let mut chunks: Vec<Vec<u8>> = Vec::new();
        match &mut self.kind {
            OutputKind::Auto { .. } => unreachable!(),
            OutputKind::Raw => chunks.push(payload.to_vec()),
            OutputKind::Lines { delimiter } => {
                let d = *delimiter;
                let mut chunk = payload.to_vec();
                chunk.push(d);
                chunks.push(chunk);
            }
            OutputKind::FMp4 { timescale, seq, prev_decode_time, output_decode_time, moov_written } => {
                if let Some(Timescale(ts)) = props.timescale() { *timescale = ts as u32; }
                if let Some(cfg) = props.codec_config() {
                    if !*moov_written {
                        chunks.push(fmp4_write::ftyp());
                        chunks.push(fmp4_write::moov(&[(*timescale, cfg)]));
                        *moov_written = true;
                    }
                }
                if *moov_written {
                    let decode_time = props.timestamp().map(|Timestamp(t)| t).unwrap_or(0);
                    let is_first = prev_decode_time.is_none();
                    let nominal_duration = prev_decode_time.map(|p| decode_time.saturating_sub(p) as u32).unwrap_or(0);
                    // Nudge the declared fragment duration by how many fragments are
                    // already queued waiting to be flushed to `file`: if the sink is
                    // falling behind, play the container's timeline a touch faster to
                    // drain the backlog before it becomes unbounded latency; if we're
                    // fully caught up (or the sink just ran dry), play it a touch
                    // slower so the next arrival doesn't immediately stutter.
                    let duration = moq_pacing::PacingThresholds::FRAGMENT_BACKLOG
                        .scale(nominal_duration as f32, self.buf.len() as f32) as u32;
                    *prev_decode_time = Some(decode_time);
                    if is_first { *output_decode_time = decode_time; }
                    let tfdt = *output_decode_time;
                    *output_decode_time += duration as u64;
                    *seq += 1;
                    let is_sync = props.video_frame_marking()
                        .map(|m| m.is_independent())
                        .unwrap_or_else(|| props.codec_config().is_some());
                    chunks.push(fmp4_write::fragment(*seq, 1, tfdt, duration, is_sync, payload));
                }
            }
        }
        for chunk in chunks { self.buf.push_back(chunk); }
    }
}

fn drain_pending(mut file: &File, buf: &mut VecDeque<Vec<u8>>, partial: &mut usize) {
    while let Some(front) = buf.front() {
        let remaining = &front[*partial..];
        if remaining.is_empty() { buf.pop_front(); *partial = 0; continue; }
        match file.write(remaining) {
            Ok(n) if n > 0 => { *partial += n; if *partial >= front.len() { buf.pop_front(); *partial = 0; } }
            _ => break,
        }
    }
}

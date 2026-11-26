/// Minimal streaming single-track fragmented MP4 demuxer.
///
/// Designed for the output of:
///   `ffmpeg ... -movflags empty_moov+default_base_moof -frag_duration 1 -f mp4 pipe:1`
///
/// Only one media track is supported; the demuxer will set `error` if the
/// input contains more than one track.  Feed raw bytes with [`FMp4Demuxer::push`];
/// pull decoded samples with [`FMp4Demuxer::next_sample`].
use std::collections::{HashMap, VecDeque};

// ── public types ──────────────────────────────────────────────────────────────

pub struct TrackInfo {
    pub timescale: u32,
    /// Generic codec configuration: `[FourCC (4 bytes)][stsd sample-entry body]`.
    ///
    /// The FourCC identifies the codec (e.g. `b"avc1"` for H.264, `b"mp4a"` for AAC).
    /// The body is everything inside the stsd sample-entry box after the 8-byte box header.
    /// Consumers use the FourCC to determine how to interpret the body.
    pub codec_config: Option<Vec<u8>>,
}

pub struct Sample {
    pub decode_time: u64,
    pub is_sync: bool,
    pub data: Vec<u8>,
}

// ── internal types ────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct TrackExtends {
    default_sample_duration: u32,
    default_sample_size: u32,
    default_sample_flags: u32,
}

struct TrunSample {
    size: u32,
    duration: u32,
    is_sync: bool,
}

struct TrackFragment {
    base_decode_time: u64,
    samples: Vec<TrunSample>,
}

// ── demuxer ───────────────────────────────────────────────────────────────────

pub struct FMp4Demuxer {
    buf: Vec<u8>,
    /// Exactly one track after a successful moov parse.
    pub track: Option<TrackInfo>,
    /// Set when the input contains more than one track.
    pub error: Option<String>,
    trex: HashMap<u32, TrackExtends>,
    pub moov_done: bool,
    pending_moof: Vec<TrackFragment>,
    pending_samples: VecDeque<Sample>,
}

impl Default for FMp4Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl FMp4Demuxer {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            track: None,
            error: None,
            trex: HashMap::new(),
            moov_done: false,
            pending_moof: Vec::new(),
            pending_samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        self.process();
    }

    pub fn next_sample(&mut self) -> Option<Sample> {
        self.pending_samples.pop_front()
    }

    fn process(&mut self) {
        loop {
            let Some((box_type, header_size, body_size)) = parse_box_header(&self.buf) else {
                break;
            };
            let total = header_size + body_size;
            if self.buf.len() < total {
                break;
            }
            let body = self.buf[header_size..total].to_vec();
            match &box_type {
                b"ftyp" | b"styp" | b"sidx" | b"free" | b"skip" => {}
                b"moov" => {
                    self.parse_moov(&body);
                    self.moov_done = true;
                }
                b"moof" => self.parse_moof(&body),
                b"mdat" => self.parse_mdat(&body),
                _ => {}
            }
            self.buf.drain(..total);
        }
    }
}

// ── moov parsing ─────────────────────────────────────────────────────────────

impl FMp4Demuxer {
    fn parse_moov(&mut self, data: &[u8]) {
        let mut tracks: Vec<TrackInfo> = Vec::new();
        iter_boxes(data, |btype, bdata| match btype {
            b"trak" => {
                if let Some(t) = parse_trak(bdata) {
                    tracks.push(t);
                }
            }
            b"mvex" => self.parse_mvex(bdata),
            _ => {}
        });

        match tracks.len() {
            0 => {}
            1 => { self.track = Some(tracks.remove(0)); }
            n => {
                self.error = Some(format!(
                    "input contains {n} tracks; only single-track fMP4 is supported"
                ));
                // Still store the first track so the demuxer can continue without crashing.
                self.track = Some(tracks.remove(0));
            }
        }
    }

    fn parse_mvex(&mut self, data: &[u8]) {
        iter_boxes(data, |btype, bdata| {
            if btype == b"trex" && bdata.len() >= 24 {
                let track_id = u32be(bdata, 4);
                self.trex.insert(track_id, TrackExtends {
                    default_sample_duration: u32be(bdata, 12),
                    default_sample_size: u32be(bdata, 16),
                    default_sample_flags: u32be(bdata, 20),
                });
            }
        });
    }
}

fn parse_trak(data: &[u8]) -> Option<TrackInfo> {
    let mut track_id = 0u32;
    let mut timescale = 0u32;
    let mut codec_config: Option<Vec<u8>> = None;

    iter_boxes(data, |btype, bdata| match btype {
        b"tkhd" => {
            if bdata.len() >= 24 {
                track_id = if bdata[0] == 1 {
                    u32be(bdata, 20)
                } else {
                    u32be(bdata, 12)
                };
            }
        }
        b"mdia" => parse_mdia(bdata, &mut timescale, &mut codec_config),
        _ => {}
    });

    if track_id == 0 { return None; }
    Some(TrackInfo { timescale, codec_config })
}

fn parse_mdia(data: &[u8], timescale: &mut u32, codec_config: &mut Option<Vec<u8>>) {
    iter_boxes(data, |btype, bdata| match btype {
        b"mdhd" => {
            if bdata.len() >= 12 {
                *timescale = if bdata[0] == 1 { u32be(bdata, 20) } else { u32be(bdata, 12) };
            }
        }
        b"minf" => parse_minf(bdata, codec_config),
        _ => {}
    });
}

fn parse_minf(data: &[u8], codec_config: &mut Option<Vec<u8>>) {
    iter_boxes(data, |btype, bdata| {
        if btype == b"stbl" { parse_stbl(bdata, codec_config); }
    });
}

fn parse_stbl(data: &[u8], codec_config: &mut Option<Vec<u8>>) {
    iter_boxes(data, |btype, bdata| {
        if btype == b"stsd" { parse_stsd(bdata, codec_config); }
    });
}

/// Extract the first sample entry from stsd as `[FourCC(4)][body]`.
fn parse_stsd(data: &[u8], codec_config: &mut Option<Vec<u8>>) {
    if data.len() < 8 { return; } // skip version+flags+entry_count
    let entries = &data[8..];
    if codec_config.is_some() { return; }
    // Take the very first sample entry — we're single-track and codec-agnostic.
    iter_boxes(entries, |btype, bdata| {
        if codec_config.is_none() {
            let mut cfg = Vec::with_capacity(4 + bdata.len());
            cfg.extend_from_slice(btype);  // FourCC
            cfg.extend_from_slice(bdata);   // stsd entry body
            *codec_config = Some(cfg);
        }
    });
}

// ── moof parsing ─────────────────────────────────────────────────────────────

impl FMp4Demuxer {
    fn parse_moof(&mut self, data: &[u8]) {
        self.pending_moof.clear();
        iter_boxes(data, |btype, bdata| {
            if btype == b"traf"
                && let Some(frag) = self.parse_traf(bdata) {
                    self.pending_moof.push(frag);
                }
        });
    }

    fn parse_traf(&self, data: &[u8]) -> Option<TrackFragment> {
        let mut track_id = 0u32;
        let mut base_decode_time = 0u64;
        let mut default_duration = 0u32;
        let mut default_size = 0u32;
        let mut default_flags = 0u32;
        let mut samples: Vec<TrunSample> = Vec::new();

        iter_boxes(data, |btype, bdata| match btype {
            b"tfhd" => {
                if bdata.len() < 8 { return; }
                let flags = u32be(bdata, 0) & 0xFFFFFF;
                track_id = u32be(bdata, 4);
                let trex = self.trex.get(&track_id).cloned().unwrap_or_default();
                let mut off = 8usize;
                if flags & 0x000001 != 0 { off += 8; }
                if flags & 0x000002 != 0 { off += 4; }
                default_duration = if flags & 0x000008 != 0 { let v = u32be(bdata, off); off += 4; v } else { trex.default_sample_duration };
                default_size = if flags & 0x000010 != 0 { let v = u32be(bdata, off); off += 4; v } else { trex.default_sample_size };
                default_flags = if flags & 0x000020 != 0 { u32be(bdata, off) } else { trex.default_sample_flags };
            }
            b"tfdt" => {
                if bdata.len() >= 4 {
                    base_decode_time = if bdata[0] == 1 && bdata.len() >= 12 {
                        u64be(bdata, 4)
                    } else if bdata.len() >= 8 {
                        u32be(bdata, 4) as u64
                    } else { 0 };
                }
            }
            b"trun" => {
                if bdata.len() < 8 { return; }
                let flags = u32be(bdata, 0) & 0xFFFFFF;
                let sample_count = u32be(bdata, 4) as usize;
                let mut off = 8usize;
                if flags & 0x000001 != 0 { off += 4; }
                let first_sample_flags = if flags & 0x000004 != 0 { let v = u32be(bdata, off); off += 4; Some(v) } else { None };
                for i in 0..sample_count {
                    if off > bdata.len() { break; }
                    let duration = if flags & 0x000100 != 0 { let v = u32be(bdata, off); off += 4; v } else { default_duration };
                    let size = if flags & 0x000200 != 0 { let v = u32be(bdata, off); off += 4; v } else { default_size };
                    let sflags = if flags & 0x000400 != 0 { let v = u32be(bdata, off); off += 4; v }
                        else if i == 0 && first_sample_flags.is_some() { first_sample_flags.unwrap() }
                        else { default_flags };
                    if flags & 0x000800 != 0 { off += 4; }
                    let is_sync = (sflags & 0x0001_0000) == 0;
                    samples.push(TrunSample { size, duration, is_sync });
                }
            }
            _ => {}
        });

        if track_id == 0 { return None; }
        Some(TrackFragment { base_decode_time, samples })
    }
}

// ── mdat parsing ─────────────────────────────────────────────────────────────

impl FMp4Demuxer {
    fn parse_mdat(&mut self, data: &[u8]) {
        let frags = std::mem::take(&mut self.pending_moof);
        let mut pos = 0;
        for frag in frags {
            let mut decode_time = frag.base_decode_time;
            for s in &frag.samples {
                let end = pos + s.size as usize;
                if end > data.len() { break; }
                self.pending_samples.push_back(Sample {
                    decode_time,
                    is_sync: s.is_sync,
                    data: data[pos..end].to_vec(),
                });
                decode_time += s.duration as u64;
                pos = end;
            }
        }
    }
}

// ── box parsing helpers ───────────────────────────────────────────────────────

pub(crate) fn parse_box_header(data: &[u8]) -> Option<([u8; 4], usize, usize)> {
    if data.len() < 8 { return None; }
    let raw_size = u32be(data, 0) as usize;
    let box_type: [u8; 4] = data[4..8].try_into().unwrap();
    let (header_size, body_size) = if raw_size == 1 {
        if data.len() < 16 { return None; }
        let ext = u64be(data, 8) as usize;
        if ext < 16 { return None; }
        (16, ext - 16)
    } else if raw_size == 0 {
        return None;
    } else {
        if raw_size < 8 { return None; }
        (8, raw_size - 8)
    };
    Some((box_type, header_size, body_size))
}

pub(crate) fn iter_boxes(mut data: &[u8], mut f: impl FnMut(&[u8; 4], &[u8])) {
    while data.len() >= 8 {
        let Some((btype, hdr, body_len)) = parse_box_header(data) else { break };
        let total = hdr + body_len;
        if total > data.len() { break; }
        f(&btype, &data[hdr..total]);
        data = &data[total..];
    }
}

#[inline]
fn u32be(data: &[u8], off: usize) -> u32 {
    if off + 4 > data.len() { return 0; }
    u32::from_be_bytes(data[off..off + 4].try_into().unwrap())
}

#[inline]
fn u64be(data: &[u8], off: usize) -> u64 {
    if off + 8 > data.len() { return 0; }
    u64::from_be_bytes(data[off..off + 8].try_into().unwrap())
}

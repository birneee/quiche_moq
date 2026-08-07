use std::fs::File;
use std::io::Read;

use log::{error, warn};
use moq_loc::{AudioConfig, LocProperties, LocProperty, Timescale, Timestamp, VideoConfig};
use moq_object_cache::{CachedObjectRef, ObjectCache, PackedObjectCache};
use quiche_mio_runner::quiche_endpoint::ClientId;
use quiche_moq::wire::control_message::subscribe::FilterType;
use quiche_moq::wire::{Location, NamespaceTrackname, RequestId, TrackAlias};

use crate::file_output::FileOutputState;
use crate::fmp4::is_fmp4;
use crate::fmp4_demux::FMp4Demuxer;

/// Codec config bytes are `[FourCC(4)][stsd sample-entry body]` (see
/// `moq_loc::VideoConfig`/`AudioConfig`); this is every video FourCC we
/// recognise, used to decide which of the two properties a track's config
/// belongs under.
pub(crate) fn is_video_fourcc(fourcc: &[u8]) -> bool {
    matches!(fourcc, b"avc1" | b"hev1" | b"hvc1" | b"av01" | b"vp08" | b"vp09")
}

pub(crate) struct LocalTrack {
    pub(crate) cache: PackedObjectCache,
    pub(crate) aliases: Vec<LocalAliasState>,
    pub(crate) file_outputs: Vec<FileOutputState>,
    pub(crate) timescale: u32,
    pub(crate) codec_config: Option<Vec<u8>>,
    /// Whether `codec_config` (once set) is an audio or video config —
    /// decides whether `push_sample` writes it as `AudioConfig` or
    /// `VideoConfig`. Meaningless while `codec_config` is `None`.
    pub(crate) is_audio: bool,
    pub(crate) group_id: u64,
    pub(crate) object_id: u64,
    pub(crate) group_start_time_us: u64,
    pub(crate) group_interval_us: u64,
}

impl LocalTrack {
    pub(crate) fn new(cache_depth: usize, cache_max_obj: usize, cache_avg_obj: usize) -> Self {
        Self {
            cache: PackedObjectCache::new(cache_depth, cache_max_obj, cache_avg_obj),
            aliases: Vec::new(),
            file_outputs: Vec::new(),
            timescale: 1_000_000,
            codec_config: None,
            is_audio: false,
            group_id: 0,
            object_id: 0,
            group_start_time_us: 0,
            group_interval_us: 0,
        }
    }

    pub(crate) fn push_sample(&mut self, payload: &[u8], is_sync: bool, decode_time_us: u64) {
        let new_group = if self.group_interval_us > 0 {
            decode_time_us >= self.group_start_time_us + self.group_interval_us
        } else {
            is_sync
        };
        if new_group && self.cache.latest().is_some() {
            self.group_id += 1;
            self.object_id = 0;
            if self.group_interval_us > 0 {
                self.group_start_time_us =
                    (decode_time_us / self.group_interval_us) * self.group_interval_us;
            }
        }

        let mut props = LocProperties::new();
        props.push(LocProperty::Timestamp(Timestamp(decode_time_us)));
        if self.cache.latest().is_none() && self.group_id == 0 && self.object_id == 0 {
            props.push(LocProperty::Timescale(Timescale(1_000_000)));
        }
        if self.object_id == 0 {
            if let Some(ref cfg) = self.codec_config {
                props.push(if self.is_audio {
                    LocProperty::AudioConfig(AudioConfig(cfg.clone()))
                } else {
                    LocProperty::VideoConfig(VideoConfig(cfg.clone()))
                });
            }
        }

        let loc = Location { group: self.group_id, object: self.object_id };
        let ext_hdrs = props.to_kvps();
        if self.cache.begin(loc, ext_hdrs, payload.len()).is_err() {
            warn!("cache full, dropping object at {:?}", loc);
            return;
        }
        if let Some(mut obj) = self.cache.latest_mut() {
            let buf = obj.unfilled();
            let n = payload.len().min(buf.len());
            buf[..n].copy_from_slice(&payload[..n]);
            obj.advance_write_pos(n);
        }
        self.object_id += 1;
    }

    pub(crate) fn push_line(&mut self, payload: &[u8], embed_timestamp: bool) {
        let loc = Location { group: self.group_id, object: 0 };
        let ext_hdrs = if embed_timestamp {
            let now_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
            let mut props = LocProperties::new();
            props.push(LocProperty::Timestamp(Timestamp(now_us)));
            props.to_kvps()
        } else {
            Default::default()
        };
        if self.cache.begin(loc, ext_hdrs, payload.len()).is_err() {
            warn!("cache full, dropping line object at {:?}", loc);
            return;
        }
        if let Some(mut obj) = self.cache.latest_mut() {
            let buf = obj.unfilled();
            let n = payload.len().min(buf.len());
            buf[..n].copy_from_slice(&payload[..n]);
            obj.advance_write_pos(n);
        }
        self.group_id += 1;
    }

    pub(crate) fn start_location_for_filter(&self, filter: FilterType) -> Option<Location> {
        match filter {
            FilterType::NextGroupStart => self.cache.latest().map(|o: CachedObjectRef| Location {
                group: o.location().group,
                object: u64::MAX,
            }),
            FilterType::LargestObject => self.cache.latest().map(|o: CachedObjectRef| o.location()),
            _ => None,
        }
    }

    /// Like `start_location_for_filter`, but for `LargestObject` also checks whether the
    /// latest cached object is still fresh enough to deliver within `delivery_timeout`.
    /// If the latest object is already older than the subscriber's requested timeout,
    /// we start fresh (returning `None`) rather than immediately serving a stale object
    /// that would be rejected with `ObjectExpired` at send time.
    pub(crate) fn start_location_for_filter_with_dt(
        &self,
        filter: FilterType,
        delivery_timeout: Option<u64>,
    ) -> Option<Location> {
        let base = self.start_location_for_filter(filter);
        if let (Some(dt_ms), FilterType::LargestObject) = (delivery_timeout, filter) {
            let dt = std::time::Duration::from_millis(dt_ms);
            if let Some(obj) = self.cache.latest() {
                if obj.received_at().elapsed() > dt {
                    return None;
                }
            }
        }
        base
    }
}

pub(crate) struct LocalAliasState {
    pub(crate) conn_id: ClientId,
    pub(crate) alias: TrackAlias,
    pub(crate) last_sent: Option<Location>,
    pub(crate) remaining: usize,
}

/// A pending incoming SUBSCRIBE for one of our own (--pub/--pub-passive) tracks,
/// collected via `MoqHandle::process_subscription_requests` for batch processing.
pub(crate) struct PendingInboundSubscription {
    pub(crate) request_id: RequestId,
    pub(crate) track: NamespaceTrackname,
    pub(crate) filter: FilterType,
    pub(crate) priority: u8,
    pub(crate) delivery_timeout: Option<u64>,
}

pub(crate) struct FileInputState {
    pub(crate) file: File,
    pub(crate) track: NamespaceTrackname,
    kind: FileInputKind,
    peek_buf: Vec<u8>,
    delimiter: u8,
}

enum FileInputKind {
    Detecting,
    FMp4 { demuxer: FMp4Demuxer },
    LineDelimited { delimiter: u8, line_buf: Vec<u8> },
}

impl FileInputState {
    pub(crate) fn new(file: File, track: NamespaceTrackname, delimiter: u8) -> Self {
        Self { file, track, kind: FileInputKind::Detecting, peek_buf: Vec::new(), delimiter }
    }

    /// Reads and ingests every byte currently available, pushing each
    /// decoded sample straight into `local`'s cache as soon as it's
    /// available — no intermediate Vec of samples. Loops until the
    /// (non-blocking) fd is exhausted — same pattern as moq_utils's
    /// `drain_fd`. A single `read()` per wakeup isn't enough: mio's epoll
    /// registration is edge-triggered, so if a burst (e.g. an fMP4
    /// moov/keyframe) exceeds one 16KB read, the leftover bytes sit
    /// unconsumed with no further READABLE notification ever firing for
    /// them — eventually filling the writer's pipe buffer and deadlocking
    /// it (observed as the whole track silently freezing).
    pub(crate) fn drain_into(&mut self, local: &mut LocalTrack, embed_timestamps: bool) {
        let mut tmp = [0u8; 16384];
        loop {
            let n = match self.file.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let data = &tmp[..n];

            if let FileInputKind::Detecting = self.kind {
                self.peek_buf.extend_from_slice(data);
                if self.peek_buf.len() < 8 && !self.peek_buf.contains(&self.delimiter) { continue; }
                if is_fmp4(&self.peek_buf) {
                    let mut demuxer = FMp4Demuxer::new();
                    demuxer.push(&std::mem::take(&mut self.peek_buf));
                    self.kind = FileInputKind::FMp4 { demuxer };
                } else {
                    let buffered = std::mem::take(&mut self.peek_buf);
                    let mut line_buf = Vec::new();
                    line_buf.extend_from_slice(&buffered);
                    self.kind = FileInputKind::LineDelimited { delimiter: self.delimiter, line_buf };
                }
            } else {
                match &mut self.kind {
                    FileInputKind::FMp4 { demuxer } => demuxer.push(data),
                    FileInputKind::LineDelimited { line_buf, .. } => line_buf.extend_from_slice(data),
                    FileInputKind::Detecting => unreachable!(),
                }
            }
            self.push_pending(local, embed_timestamps);
        }
        self.push_pending(local, embed_timestamps);
    }

    fn push_pending(&mut self, local: &mut LocalTrack, embed_timestamps: bool) {
        match &mut self.kind {
            FileInputKind::FMp4 { demuxer } => {
                if let Some(ref err) = demuxer.error { error!("fMP4 demux error: {err}"); return; }
                if local.codec_config.is_none()
                    && let Some(ti) = demuxer.track.as_ref()
                    && let Some(cfg) = ti.codec_config.as_ref()
                {
                    let is_audio = !is_video_fourcc(&cfg[..4]);
                    local.is_audio = is_audio;
                    local.group_interval_us = if is_audio { 1_000_000 } else { 0 };
                    local.timescale = ti.timescale;
                    local.codec_config = Some(cfg.clone());
                }
                let timescale = local.timescale.max(1) as u64;
                while let Some(s) = demuxer.next_sample() {
                    let us = (s.decode_time * 1_000_000) / timescale;
                    local.push_sample(&s.data, s.is_sync, us);
                }
            }
            FileInputKind::LineDelimited { delimiter, line_buf } => {
                let sep = *delimiter;
                while let Some(pos) = line_buf.iter().position(|&b| b == sep) {
                    let line: Vec<u8> = line_buf.drain(..pos).collect();
                    line_buf.drain(..1);
                    if !line.is_empty() { local.push_line(&line, embed_timestamps); }
                }
            }
            FileInputKind::Detecting => {}
        }
    }
}

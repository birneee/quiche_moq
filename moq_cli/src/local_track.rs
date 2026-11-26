use std::collections::HashMap;
use quiche_moq::SubscribeOptions;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use log::{error, warn};
use moq_loc::{AudioConfig, LocProperties, LocProperty, Timescale, Timestamp, VideoConfig};
use moq_object_cache::{CachedObjectRef, NewObject, ObjectCache, ObjectState, PackedObjectCache};
use quiche_mio_runner::quiche_endpoint::ClientId;
use quiche_moq::Request;
use quiche_moq::request::Subscribe;
use quiche_moq::wire::control_message::subscribe::LocationFilter;
use quiche_moq::wire::{PublishDoneStatus, DEFAULT_PUBLISHER_PRIORITY, KeyValuePair, KeyValuePairs, VarintKey, Location, NamespaceTrackname, RequestId, TrackAlias};

use crate::file_output::FileOutputState;
use crate::fmp4::is_fmp4;
use crate::fmp4_demux::FMp4Demuxer;
use crate::cache_to_moq::{self, CacheToMoq};

/// Codec config bytes are `[FourCC(4)][stsd sample-entry body]` (see
/// `moq_loc::VideoConfig`/`AudioConfig`); this is every video FourCC we
/// recognise, used to decide which of the two properties a track's config
/// belongs under.
pub(crate) fn is_video_fourcc(fourcc: &[u8]) -> bool {
    matches!(fourcc, b"avc1" | b"hev1" | b"hvc1" | b"av01" | b"vp08" | b"vp09")
}

fn unix_now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64
}

/// Object property: Unix-microsecond publish time of an fMP4 sample
/// (`--timestamp`). Even → varint.
/// - LOC `Timestamp` stays the decode time, media time can't be mapped to wall clock
/// - private ID, ignored by unaware receivers
pub(crate) const PUBLISH_TIME_HEADER_ID: VarintKey = VarintKey::new(0xA000);

pub(crate) struct LocalTrack {
    pub(crate) cache: PackedObjectCache,
    /// MoQ subscriptions feeding this cache — empty for a purely local
    /// track (`--pub`). Non-empty alongside non-empty `aliases` = relay.
    pub(crate) subscriptions: Vec<TrackSubscription>,
    pub(crate) aliases: Vec<LocalAliasState>,
    /// Downstream SUBSCRIBEs waiting on `subscriptions` to resolve before
    /// they can be answered — see `PendingSubscribe`.
    pub(crate) pending_subscribes: Vec<PendingSubscribe>,
    /// MAX_CACHE_DURATION (draft-18 §12.3) for SUBSCRIBE_OK — from `--pub`'s
    /// `max_cache_duration=`, or learned from an upstream SUBSCRIBE_OK if relayed.
    pub(crate) max_cache_duration: Option<u64>,
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
    /// Present exactly when this track has a `#race` `--sub` template —
    /// probe/leader-switch bookkeeping for `crate::race`.
    pub(crate) race: Option<RaceState>,
    /// Running totals for the shutdown-time summary (`crate::node`).
    /// - counted at object-header arrival, not full-payload completion — a rough
    ///   rate/count summary, not a delivery-verified total
    /// - an object arriving from several sources is counted once per source
    pub(crate) stats: TrackStats,
    /// Created on demand to relay a downstream SUBSCRIBE (no `--pub`/`--sub`
    /// of its own): never resumed after losing its upstream, removed and
    /// unsubscribed upstream once no subscriber is left (`unwanted`).
    pub(crate) relayed: bool,
    /// Its own Objects go out in datagrams (`--pub` with `&datagram`).
    pub(crate) datagram: bool,
    /// Objects before the upstream subscriptions' start, fetched for subscribers starting earlier.
    pub(crate) fill: CacheFill,
}

/// Upstream FETCHes into the cache for what subscribers want before the upstream subscription's
/// start (draft-ietf-moq-transport-19 §2.3.1), driven by `crate::node::fill`.
#[derive(Default)]
pub(crate) struct CacheFill {
    /// The earliest Location a forwarding subscriber starts at.
    pub(crate) wanted_from: Option<Location>,
    /// The earliest Location fetched so far.
    pub(crate) fetched_from: Option<Location>,
    pub(crate) fetches: Vec<UpstreamFill>,
}

impl CacheFill {
    pub(crate) fn want(&mut self, from: Location) {
        self.wanted_from = Some(self.wanted_from.map_or(from, |wanted| wanted.min(from)));
    }

    /// Whether upstream FETCHes are running, PUBLISH_DONE waits for them.
    pub(crate) fn running(&self) -> bool {
        !self.fetches.is_empty()
    }
}

/// An upstream FETCH whose objects go into the cache.
pub(crate) struct UpstreamFill {
    pub(crate) source: ClientId,
    pub(crate) upstream: quiche_moq::Fetching,
    /// Where the fetched range ends, exclusive: its Groups before this one are complete.
    pub(crate) until: Location,
    pub(crate) accepted: bool,
    /// When it is given up, its FETCH cancelled (draft-ietf-moq-transport-19 §5.2).
    pub(crate) deadline: Instant,
    /// The object whose payload is read: its length, and how much of it was read.
    pub(crate) reading: Option<(Location, usize, usize)>,
    /// The last Object fetched of each Subgroup, `(group, subgroup)`.
    pub(crate) last_of_subgroup: HashMap<(u64, u64), Location>,
}

#[derive(Default)]
pub(crate) struct TrackStats {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    pub(crate) first_at: Option<Instant>,
    pub(crate) last_at: Option<Instant>,
    /// Subgroup streams canceled rather than completed: upstream reset it
    /// mid-payload, or we declined it via `cancel_recv_obj` (cache full,
    /// Location mismatch). Not counted in `objects`/`bytes`.
    pub(crate) canceled: u64,
}

impl TrackStats {
    /// An object received.
    pub(crate) fn record(&mut self, payload_len: usize) {
        self.objects += 1;
        self.bytes += payload_len as u64;
        let now = Instant::now();
        self.first_at.get_or_insert(now);
        self.last_at = Some(now);
    }
}

impl LocalTrack {
    pub(crate) fn new(cache_depth: usize, cache_max_obj: usize, cache_avg_obj: usize, max_cache_duration: Option<u64>) -> Self {
        Self {
            cache: PackedObjectCache::new(cache_depth, cache_max_obj, cache_avg_obj),
            subscriptions: Vec::new(),
            aliases: Vec::new(),
            pending_subscribes: Vec::new(),
            max_cache_duration,
            file_outputs: Vec::new(),
            timescale: 1_000_000,
            codec_config: None,
            is_audio: false,
            group_id: 0,
            object_id: 0,
            group_start_time_us: 0,
            group_interval_us: 0,
            race: None,
            stats: TrackStats::default(),
            relayed: false,
            datagram: false,
            fill: CacheFill::default(),
        }
    }

    pub(crate) fn push_sample(&mut self, payload: &[u8], is_sync: bool, decode_time_us: u64, embed_publish_time: bool) {
        let new_group = if self.group_interval_us > 0 {
            decode_time_us >= self.group_start_time_us + self.group_interval_us
        } else {
            is_sync
        };
        let latest = self.cache.latest().map(|o| o.location());
        let is_first_object_ever = latest.is_none();
        if new_group && let Some(last_of_group) = latest {
            self.mark_end_of_group(last_of_group);
            self.group_id += 1;
            self.object_id = 0;
            if self.group_interval_us > 0 {
                self.group_start_time_us =
                    (decode_time_us / self.group_interval_us) * self.group_interval_us;
            }
        }

        let mut props = LocProperties::new();
        props.push(LocProperty::Timestamp(Timestamp(decode_time_us)));
        if is_first_object_ever {
            props.push(LocProperty::Timescale(Timescale(1_000_000)));
        }
        if self.object_id == 0
            && let Some(ref cfg) = self.codec_config {
                props.push(if self.is_audio {
                    LocProperty::AudioConfig(AudioConfig(cfg.clone()))
                } else {
                    LocProperty::VideoConfig(VideoConfig(cfg.clone()))
                });
            }

        let mut properties = props.to_kvps();
        if embed_publish_time {
            properties.insert(KeyValuePair::Varint(PUBLISH_TIME_HEADER_ID, unix_now_us()));
        }

        let loc = Location { group: self.group_id, object: self.object_id };
        if !self.push_object(loc, false, properties, payload) {
            warn!("cache full, dropping object at {:?}", loc);
            return;
        }
        self.object_id += 1;
    }

    pub(crate) fn push_line(&mut self, payload: &[u8], embed_timestamp: bool) {
        let loc = Location { group: self.group_id, object: 0 };
        let properties = if embed_timestamp {
            let mut props = LocProperties::new();
            props.push(LocProperty::Timestamp(Timestamp(unix_now_us())));
            props.to_kvps()
        } else {
            Default::default()
        };
        // Every line is its own single-object Group/Subgroup.
        if !self.push_object(loc, true, properties, payload) {
            warn!("cache full, dropping line object at {:?}", loc);
            return;
        }
        self.group_id += 1;
    }

    /// Begins a new cached object at `loc` and writes `payload` into it,
    /// flagging any alias whose in-flight subgroup pointed at whatever got
    /// evicted to make room. `false` means the cache was full and nothing
    /// was written — the caller logs its own context-specific warning.
    fn push_object(&mut self, loc: Location, end_of_subgroup: bool, properties: KeyValuePairs, payload: &[u8]) -> bool {
        // Locally published tracks always use a single Subgroup per Group.
        let object = NewObject { publisher_priority: DEFAULT_PUBLISHER_PRIORITY, end_of_subgroup, properties, ..NewObject::new(loc, payload.len()) };
        let evicted = match self.cache.begin(object) {
            Ok(evicted) => evicted,
            Err(_) => return false,
        };
        self.mark_evicted(&evicted);
        if end_of_subgroup {
            self.cache.mark_end_of_group(loc);
        }
        if self.datagram && let Some(mut object) = self.cache.find_mut(loc) {
            object.set_datagram();
        }
        if let Some(mut obj) = self.cache.latest_mut() {
            let buf = obj.unfilled();
            let n = payload.len().min(buf.len());
            buf[..n].copy_from_slice(&payload[..n]);
            obj.advance_write_pos(n);
        }
        true
    }

    /// If `begin()` just evicted objects, flag any alias with an in-flight
    /// subgroup still keyed to one of them — its bytes may already be
    /// sitting unsent in the transport layer regardless of whether the app
    /// considers the object fully handed off (`remaining == 0`).
    fn mark_evicted(&mut self, evicted: &[Location]) {
        cache_to_moq::mark_evicted(self.aliases.iter_mut().map(|a| &mut a.state), evicted);
    }

    /// Marks the last-pushed object as ending its Subgroup — call once the
    /// input is fully drained (EOF), since `push_sample`'s group-boundary
    /// detection only fires retroactively on the *next* sample and so can
    /// never mark the track's final group on its own.
    pub(crate) fn close(&mut self) {
        if let Some(loc) = self.cache.latest().map(|o| o.location()) {
            self.mark_end_of_group(loc);
        }
    }

    /// One Subgroup per Group: its last Object ends both.
    fn mark_end_of_group(&mut self, loc: Location) {
        self.cache.mark_end_of_subgroup(loc);
        self.cache.mark_end_of_group(loc);
    }

    /// A relayed track nobody downstream wants anymore.
    pub(crate) fn unwanted(&self) -> bool {
        self.relayed && self.aliases.is_empty() && self.pending_subscribes.is_empty()
    }

    /// Whether relayed upstream subscriptions deliver: while a subscriber, answered, still waiting or
    /// one of the Forward States `more` not here yet, is in Forward State 1, with `prewarm` always
    /// (draft-ietf-moq-transport-19 §9.2); `None` without subscribers.
    pub(crate) fn upstream_forward(&self, prewarm: bool, more: impl IntoIterator<Item = bool>) -> Option<bool> {
        let mut forwards = self.aliases.iter().map(|a| a.forward)
            .chain(self.pending_subscribes.iter().map(|p| p.request.message().forward))
            .chain(more)
            .peekable();
        forwards.peek()?;
        Some(prewarm || forwards.any(|forward| forward))
    }

    /// Nothing feeds it, serves from it or waits on it.
    pub(crate) fn unused(&self) -> bool {
        self.subscriptions.is_empty() && self.aliases.is_empty() && self.pending_subscribes.is_empty()
            && self.file_outputs.is_empty() && self.race.is_none()
    }

    /// A `#race` standby could take over a lost leader.
    pub(crate) fn has_race_standby(&self) -> bool {
        self.race.as_ref().is_some_and(RaceState::has_standby)
    }

    /// Location of the latest cached object, `None` if empty.
    pub(crate) fn largest(&self) -> Option<Location> {
        self.cache.latest().map(|o: CachedObjectRef| o.location())
    }

    /// Location a new source continues at, `None` if the cache is empty.
    /// - first object still `Progressing`, else right after the latest one
    pub(crate) fn resume_location(&self) -> Option<Location> {
        self.cache.objects_after(None)
            .find(|obj| obj.state() == ObjectState::Progressing)
            .map(|obj| obj.location())
            .or_else(|| self.next_location())
    }

    /// Location right after the latest cached object, `None` if empty.
    pub(crate) fn next_location(&self) -> Option<Location> {
        self.largest().map(Location::next)
    }
}

/// Locations passing a SUBSCRIPTION_FILTER (draft-16 §9.2.2.5).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FilterRange {
    /// Start Location, inclusive. `None` without content (the draft defines
    /// it as `{0, 0}` then, equivalent to no start).
    pub(crate) start: Option<Location>,
    /// `AbsoluteRange`'s End Group, inclusive.
    pub(crate) end_group: Option<u64>,
}

impl FilterRange {
    /// Resolves `filter` relative to `largest`, the Largest Object reported
    /// to the subscriber (§5.1.2).
    pub(crate) fn resolve(filter: LocationFilter, largest: Option<Location>) -> Self {
        let start = match filter {
            LocationFilter::NextGroupStart => largest.map(|l| Location { group: l.group + 1, object: 0 }),
            LocationFilter::LargestObject => largest.map(Location::next),
            LocationFilter::AbsoluteStart { start } | LocationFilter::AbsoluteRange { start, .. } => Some(start),
        };
        Self { start, end_group: filter.end_group() }
    }

    pub(crate) fn contains(&self, loc: Location) -> bool {
        self.start.is_none_or(|start| loc >= start) && self.end_group.is_none_or(|end| loc.group <= end)
    }
}

pub(crate) struct LocalAliasState {
    pub(crate) conn_id: ClientId,
    pub(crate) alias: TrackAlias,
    /// For `PUBLISH_DONE` if `subscriptions` all disappear (`publish_done` below).
    pub(crate) request_id: RequestId,
    /// SUBSCRIBER_PRIORITY, egress visit order (see `forward_local_tracks`).
    pub(crate) priority: u8,
    /// PUBLISH_DONE status to end this subscriber with, e.g. once
    /// `subscriptions` emptied having been non-empty — the finalize pass
    /// sends it and drops the alias. Never set for a local-only track.
    pub(crate) publish_done: Option<PublishDoneStatus>,
    /// This subscriber's Forward State (draft-ietf-moq-transport-19 §10.2.17). `false` means the
    /// SUBSCRIBE that created this alias had `forward=0` — a probe from a
    /// `#race` subscriber (see `crate::race`), or any
    /// other peer legitimately using Forward State — so `forward_local_alias`
    /// must not send it any Objects.
    pub(crate) forward: bool,
    /// The Largest Location reported when it started forwarding, a Joining FETCH ends there
    /// (draft-ietf-moq-transport-18 §10.12.2.1).
    pub(crate) joining_location: Option<Location>,
    /// Subgroup-forwarding state — see `cache_to_moq::CacheToMoq`.
    pub(crate) state: CacheToMoq,
}

impl LocalAliasState {
    /// A new Forward State and filter from the subscriber, a relative filter against `largest`.
    pub(crate) fn update(&mut self, largest: Option<Location>, forward: Option<bool>, filter: Option<LocationFilter>) {
        if let Some(filter) = filter { self.state.refilter(FilterRange::resolve(filter, largest)); }
        if let Some(forward) = forward {
            // draft-19 §11.4.3: stopping mid-Subgroup for anything but the
            // subscription's own end resets the stream, a FIN would claim
            // every object was delivered. Done by the next `forward_local_alias`.
            self.state.cancel_unfinished |= !forward && self.forward;
            // draft-ietf-moq-transport-18 §5.1: the Largest Location of an update to Forward 1 joins anew
            if forward && !self.forward { self.joining_location = largest; }
            self.forward = forward;
        }
    }
}

/// One MoQ subscription feeding this track's cache — input-side counterpart
/// to `LocalAliasState`. Non-empty alongside non-empty `LocalAliasState`s = relay.
pub(crate) struct TrackSubscription {
    pub(crate) client_id: ClientId,
    pub(crate) request_id: Option<RequestId>,
    pub(crate) track_alias: Option<TrackAlias>,
    /// True for passive --sub-triggered subscriptions: don't send SUBSCRIBE,
    /// wait for the peer's PUBLISH instead.
    pub(crate) passive: bool,
    pub(crate) options: SubscribeOptions,
    /// Objects with header read and payload pending, as `(location, payload_len)`.
    /// - several at once: subgroup streams are independent
    pub(crate) in_progress: Vec<(Location, usize)>,
    /// Whether this is the current leader of a `#race` template — see
    /// `RaceState`. `crate::race::resolve_race` moves it to the winner.
    pub(crate) race: bool,
    /// Made for this node's subscribers, not a `--sub`: its Forward State follows
    /// theirs (`sync_relayed_forward`).
    pub(crate) relayed: bool,
    /// Our REQUEST_UPDATE of its Forward State awaiting an answer.
    pub(crate) forward_update: Option<RequestId>,
    /// Its first Location, its filter resolved by the SUBSCRIBE_OK.
    pub(crate) start: Option<Location>,
}

impl TrackSubscription {
    pub(crate) fn new(client_id: ClientId, passive: bool, options: SubscribeOptions) -> Self {
        Self {
            client_id,
            request_id: None,
            track_alias: None,
            passive,
            options,
            in_progress: Vec::new(),
            race: false,
            relayed: false,
            forward_update: None,
            start: None,
        }
    }

    /// An upstream subscription for this node's subscribers, see `relayed`.
    pub(crate) fn relayed(client_id: ClientId, options: SubscribeOptions) -> Self {
        Self { relayed: true, ..Self::new(client_id, false, options) }
    }

    /// A relayed subscription for an incoming PUBLISH accepted in Forward State `forward`: the peer
    /// chose the alias, we just need to start reading objects.
    pub(crate) fn from_accepted_publish(client_id: ClientId, request_id: RequestId, alias: TrackAlias, forward: bool) -> Self {
        let options = SubscribeOptions { forward, ..SubscribeOptions::default() };
        Self { request_id: Some(request_id), track_alias: Some(alias), relayed: true, ..Self::new(client_id, true, options) }
    }
}

/// A non-leader `#race` candidate's persistent Forward=0 subscription.
/// - kept established so `crate::race::trigger_race` can flip it to Forward=1
///   immediately, no fresh SUBSCRIBE round trip, when a race fires (`SigAction::RaceProbe`)
pub(crate) struct RaceCandidate {
    pub(crate) request_id: RequestId,
    /// `None` until the initial SUBSCRIBE_OK resolves.
    pub(crate) track_alias: Option<TrackAlias>,
}

/// A `#race` track's in-flight, `SigAction::RaceProbe`-triggered race.
/// - see `crate::race::trigger_race`/`resolve_race`
/// - every candidate flipped to Forward=1 races the incumbent leader (already
///   Forward=1 at `target`; `RaceMode::Data` needs no entry for it) to be
///   first to prove — via `REQUEST_UPDATE_OK`'s `LARGEST_OBJECT` or an actual Object —
///   it has content at or after `target`
pub(crate) struct ActiveRace {
    /// Location every participant was told to start from: the first object
    /// not fully received when the race began.
    pub(crate) target: Location,
    /// Non-leader candidates racing: `client_id` -> the forward=1
    /// `REQUEST_UPDATE`'s Request ID, for `poll_request_update_response`.
    /// `RaceMode::Keep`/`Switch`: includes the probed leader.
    pub(crate) participants: HashMap<ClientId, RequestId>,
    /// `RaceMode::Keep`/`Switch`: the Forward=1 participant, kept if nobody else wins.
    pub(crate) forwarder: Option<ClientId>,
    /// Participants without a result by then leave the race.
    pub(crate) deadline: Option<Instant>,
}

/// Per-track bookkeeping for a `#race` `--sub` template — see `LocalTrack::race`.
/// `sync_sub_track_templates` refreshes `candidates` whenever new
/// announcement/upstream info arrives.
#[derive(Default)]
pub(crate) struct RaceState {
    /// Other known candidates besides the current leader.
    pub(crate) candidates: Vec<ClientId>,
    /// Persistent Forward=0 subscriptions, one per candidate (see `RaceCandidate`).
    /// - never unsubscribed while still a candidate, only Forward/filter-updated in place
    /// - leader excluded: its request_id/track_alias already live on its own
    ///   `TrackSubscription`, which is what actually pulls Objects into `LocalTrack::cache`
    pub(crate) established: HashMap<ClientId, RaceCandidate>,
    /// `Some` from the moment `trigger_race` fires a race until
    /// `resolve_race` decides a winner.
    pub(crate) active: Option<ActiveRace>,
    /// See `crate::args::RaceMode`.
    pub(crate) mode: crate::args::RaceMode,
    /// The template's SUBSCRIBE options, see [`Self::standby_options`].
    pub(crate) options: SubscribeOptions,
}

impl RaceState {
    /// A standby forwards nothing until it takes over, from the largest object then.
    pub(crate) fn standby_options(&self) -> SubscribeOptions {
        SubscribeOptions { filter: LocationFilter::LargestObject, forward: false, ..self.options }
    }

    /// `cid` becomes the leader, no longer a standby candidate.
    pub(crate) fn promote(&mut self, cid: ClientId) {
        self.established.remove(&cid);
        self.candidates.retain(|&c| c != cid);
    }

    /// A standby that could take over now (its SUBSCRIBE was answered).
    pub(crate) fn has_standby(&self) -> bool {
        self.established.values().any(|c| c.track_alias.is_some())
    }
}

/// Cancels the partially received `loc` a subscription stopped filling,
/// unless another one still fills it (`#multi`, `#race`) or it's no longer
/// in progress. The forwarder then resets downstream streams sending it.
pub(crate) fn cancel_abandoned(cache: &mut PackedObjectCache, subscriptions: &[TrackSubscription], stats: &mut TrackStats, loc: Location) {
    if subscriptions.iter().any(|s| s.in_progress.iter().any(|&(l, _)| l == loc)) { return; }
    let Some(mut obj) = cache.find_mut(loc) else { return };
    if obj.state() != ObjectState::Progressing { return; }
    obj.cancel();
    stats.canceled += 1;
}

/// A downstream SUBSCRIBE for a relayed track, answered once an upstream
/// subscription resolves: SUBSCRIBE_OK carries the track's Extension Headers
/// (draft-16 §8.6), e.g. MAX_CACHE_DURATION. See `drive_pending_subscribes`.
pub(crate) struct PendingSubscribe {
    pub(crate) client_id: ClientId,
    pub(crate) request: Request<Subscribe>,
    /// Largest Object when deferred: the answer reports it, so nothing that
    /// arrives while waiting is skipped by the subscriber's filter.
    pub(crate) largest: Option<Location>,
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

    /// Reads until the fd has nothing left, pushing each sample into
    /// `local`'s cache. Whether the input ended (`read` returned `Ok(0)`).
    /// - all, not one read: mio's epoll is edge-triggered, leftover bytes
    ///   would get no further READABLE and stall the writer
    pub(crate) fn drain_into(&mut self, local: &mut LocalTrack, embed_timestamps: bool) -> bool {
        let mut tmp = [0u8; 16384];
        let eof = loop {
            let n = match self.file.read(&mut tmp) {
                Ok(0) => break true,
                Err(_) => break false,
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
        };
        self.push_pending(local, embed_timestamps);
        if eof { local.close(); }
        eof
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
                    local.push_sample(&s.data, s.is_sync, us, embed_timestamps);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn props(track: &LocalTrack, group: u64, object: u64) -> Vec<LocProperty> {
        let obj = track.cache.find(Location { group, object }).unwrap();
        LocProperties::from_kvps(obj.properties()).iter().cloned().collect()
    }

    fn has_config(track: &LocalTrack, group: u64, object: u64) -> bool {
        props(track, group, object).iter().any(|p| matches!(p, LocProperty::VideoConfig(_) | LocProperty::AudioConfig(_)))
    }

    fn groups(track: &LocalTrack) -> Vec<u64> {
        track.cache.objects_after(None).map(|obj| obj.location().group).collect()
    }

    #[test]
    fn only_first_object_has_timescale() {
        let mut track = LocalTrack::new(16, 64, 16, None);
        for i in 0..3 {
            track.push_sample(b"frame", i == 0, i * 1000, false);
        }
        assert!(props(&track, 0, 0).iter().any(|p| matches!(p, LocProperty::Timescale(_))));
        assert!(!props(&track, 0, 1).iter().any(|p| matches!(p, LocProperty::Timescale(_))));
    }

    /// A subscriber joining at any group start gets the codec config.
    #[test]
    fn codec_config_only_on_group_start() {
        let mut track = LocalTrack::new(16, 64, 16, None);
        track.codec_config = Some(b"avc1config".to_vec());
        for i in 0..6 {
            track.push_sample(b"frame", i % 3 == 0, i * 1000, false);
        }
        assert!(has_config(&track, 0, 0));
        assert!(has_config(&track, 1, 0));
        assert!(!has_config(&track, 0, 1));
        assert!(!has_config(&track, 1, 2));
    }

    #[test]
    fn video_group_starts_at_sync_sample() {
        let mut track = LocalTrack::new(16, 64, 16, None);
        for (i, sync) in [true, false, false, true, false, true].into_iter().enumerate() {
            track.push_sample(b"frame", sync, i as u64 * 1000, false);
        }
        assert_eq!(groups(&track), [0, 0, 0, 1, 1, 2]);
    }

    /// Audio has no sync samples, groups are cut by decode time instead.
    #[test]
    fn audio_group_every_interval() {
        let mut track = LocalTrack::new(256, 64, 16, None);
        track.group_interval_us = 1_000_000;
        for i in 0..150u64 {
            track.push_sample(b"frame", true, i * 20_000, false);
        }
        let groups = groups(&track);
        assert_eq!(groups.iter().filter(|&&g| g == 0).count(), 50, "1 s of 20 ms frames");
        assert_eq!(groups.iter().filter(|&&g| g == 1).count(), 50);
        assert_eq!(*groups.last().unwrap(), 2);
    }
}

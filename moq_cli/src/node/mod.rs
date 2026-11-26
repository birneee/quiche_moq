use std::collections::{HashMap, HashSet};
use quiche_moq::SubscribeOptions;
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use boring::ssl::{SslContextBuilder, SslMethod};
use log::{error, info};
use mio::Interest;
use url::Url;

use moq_object_cache::{begin_or_join, fold, put_datagram, JoinError, NewObject, ObjectCache, PackedObjectCache};
use quiche_endpoint_utils::SignalSource;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::{
    ClientId, ConnMap, Endpoint, EndpointConfig, ServerConfig, make_qlog_writer, quiche,
};
use quiche_mio_runner::Socket;
use quiche_moq as moq;
use quiche_moq::wire::control_message::subscribe::LocationFilter;
use quiche_moq::wire::control_message::PublishOkMessage;
use quiche_moq::wire::{PublishDoneStatus, SessionCloseCode};
use quiche_moq::wire::{DEFAULT_SUBSCRIBER_PRIORITY, Location, Namespace, NamespaceTrackname, RequestErrorCode, RequestId, TrackAlias};
use quiche_moq::request::{Publish, PublishNamespace, RequestUpdate, Subscribe, SubscribeNamespace, SubscribeTracks};
use quiche_moq::{Event, IncomingRequest, Request, RequestKind, SubscribeOk, Transport};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use quiche_utils::cert::load_or_generate_keys;
use quiche_utils::keepalive_handler::KeepaliveHandler;

use crate::conn::{ClientActivePubTrack, ConnAppData, Handles, NamespaceSubscriber, TracksSubscriber};
use crate::file_output::FileOutputState;
use crate::local_track::{cancel_abandoned, FileInputState, FilterRange, LocalAliasState, LocalTrack, PendingSubscribe, RaceState, TrackStats, TrackSubscription};
use crate::cache_to_moq::{self, CacheToMoq};
use crate::race::{ensure_race_candidates_established, poll_active_race, replace_lost_leader, resolve_race, trigger_race};
use crate::args::QuicSettings;
use crate::{Args, PubTarget, SigAction, TrackPattern};

mod connections;
mod egress;
mod fetch;
mod fill;
mod ingest;
mod requests;
mod routing;
mod setup;

use connections::*;
use egress::*;
use fetch::*;
use fill::*;
use ingest::*;
use requests::*;
use routing::*;
use setup::*;

pub use setup::run;
#[cfg(test)]
pub use setup::{run_stoppable, StopHandle};

type Runner = runner::Runner<ConnAppData, AppData, i32>;

/// Sentinel external-event value for the SIGINT/SIGTERM signal source.
/// Real file descriptors registered via `register_external` are always >= 0.
const SHUTDOWN_EVENT: i32 = -1;

/// Sentinel external-event value for the SIGUSR1 signal source.
const USR1_EVENT: i32 = -2;

/// Sentinel external-event value for the SIGUSR2 signal source.
const USR2_EVENT: i32 = -3;

/// Template for a concrete `--sub` track, used by `sync_sub_track_templates`
/// to add a `TrackSubscription` on the relevant `LocalTrack` for each
/// currently-known candidate connection (namespace announcer or `#upstream`).
#[derive(Clone)]
struct SubTrackTemplate {
    track: NamespaceTrackname,
    passive: bool,
    multi: bool,
    /// See `crate::args::SubArg::race`. Mutually exclusive with `multi`.
    race: bool,
    /// See `crate::args::SubArg::race_mode`.
    race_mode: crate::args::RaceMode,
    options: SubscribeOptions,
}

/// Ends our subscription `request_id` on `cid`, if that connection is still up.
pub(crate) fn unsubscribe_upstream(conns: &mut ConnMap<ConnAppData>, cid: ClientId, request_id: Option<RequestId>) {
    if let Some(request_id) = request_id
        && let Some(conn) = conns.get_mut(cid)
    {
        conn.app_data.responses.forget(request_id);
        let subscription = conn.app_data.handles.subscriptions.remove(&request_id);
        if let Some(subscription) = subscription
            && let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn)
        {
            moq.session.unsubscribe(subscription, &mut moq.t);
        }
    }
}

/// `cid`'s session and our handles on it, if that connection is still up.
pub(crate) fn conn_for(conns: &mut ConnMap<ConnAppData>, cid: ClientId) -> Option<(MoqHandle<'_>, &mut Handles)> {
    let conn = conns.get_mut(cid)?;
    let moq = conn.app_data.moq_helper.moq_handle(&mut conn.conn)?;
    Some((moq, &mut conn.app_data.handles))
}
/// Answers to our requests on `cid`, if that connection is still up.
pub(crate) fn responses_for(conns: &mut ConnMap<ConnAppData>, cid: ClientId) -> Option<&mut crate::conn::Responses> {
    Some(&mut conns.get_mut(cid)?.app_data.responses)
}
struct AppData {
    /// `--pub` namespace/global wildcard patterns: any incoming SUBSCRIBE
    /// for a track under a matched namespace is dynamically accepted and
    /// served locally (a `LocalTrack` entry is created lazily), even
    /// without a concrete `--pub` registration for it.
    pub_passive_wildcards: Vec<TrackPattern>,
    /// `--sub` namespace/global wildcard patterns: matching SUBSCRIBEs with
    /// a known namespace (announced or `#upstream`) get relay-forwarded.
    /// Non-empty also means this node trusts PUBLISH_NAMESPACE announcements
    /// (populates `namespaces` below).
    sub_passive_wildcards: Vec<TrackPattern>,
    relay_namespace: Vec<TrackPattern>,
    cache_depth: usize,
    cache_max_obj: usize,
    cache_avg_obj: usize,
    local_tracks: HashMap<NamespaceTrackname, LocalTrack>,
    local_namespaces: HashSet<Namespace>,
    file_inputs: Vec<FileInputState>,
    // Connection(s) currently announcing each namespace — trusted per `want`
    // in answer_publish_namespace, or unconditionally for #upstream. One namespace
    // can have several announcers. Used for relay routing and `--sub`
    // candidate selection (sync_sub_track_templates).
    namespaces: HashMap<Namespace, HashSet<ClientId>>,
    /// SUBSCRIBEs without a source yet, waiting for their RENDEZVOUS_TIMEOUT.
    rendezvous: Vec<Rendezvous>,
    /// The subscribers' FETCHes being answered.
    fetches: Vec<DownstreamFetch>,
    // Every #upstream connection (--connect or --listen) — trusted
    // unconditionally, candidate source for `--sub`.
    upstream_cids: HashSet<ClientId>,
    // From --listen ...#upstream: accepted connections join upstream_cids on first sight.
    listen_upstream: bool,
    // Tracks actively PUBLISHed (0x1D) to all connections (--pub ns--track).
    active_pub_tracks: Vec<NamespaceTrackname>,
    // Concrete --sub templates; candidates resolved dynamically by
    // sync_sub_track_templates, not seeded blindly per connection.
    sub_track_templates: Vec<SubTrackTemplate>,
    // Per-connection track state (keyed by ClientId — separate from ConnAppData
    // to avoid split-borrow conflicts when moq_handle borrows conn.conn).
    client_active_pub_tracks: HashMap<ClientId, Vec<ClientActivePubTrack>>,
    // Set when SIGINT/SIGTERM is received; post_handle_recvs closes sessions then stops the runner.
    shutdown_requested: bool,
    // SIGUSR1/SIGUSR2 received; post_handle_recvs runs whichever `SigAction`s
    // are assigned (`sigusr1_actions`/`sigusr2_actions`), then clears these.
    usr1_requested: bool,
    usr2_requested: bool,
    // From --sigusr1/--sigusr2: which actions each signal runs — see `SigAction`.
    sigusr1_actions: Vec<SigAction>,
    sigusr2_actions: Vec<SigAction>,
    // From --padding-stream-connect-only: restrict `TogglePaddingStream` to
    // --connect/#upstream connections, skipping --listen ones.
    padding_stream_connect_only: bool,
    /// From --prewarm: relayed upstream subscriptions deliver even if no subscriber has Forward 1.
    prewarm: bool,
    // From --unpub: withdraw (PUBLISH_NAMESPACE_DONE) these namespaces on
    // every connection that actually announced them (see
    // `ConnAppData::announced_namespaces`) when the `Unpub` action fires.
    unpub_namespaces: Vec<Namespace>,
    // Owns the SIGUSR1/SIGUSR2 signalfds so on_file_event can drain them when
    // USR1_EVENT/USR2_EVENT fires. Registered via the non-owning
    // `register_external` (see run()); kept alive here instead.
    sigusr1_source: Option<SignalSource>,
    sigusr2_source: Option<SignalSource>,
    // From --timestamp. on_file_event (where line-delimited samples are
    // pushed) is a non-capturing `fn` too, so it's read from here.
    embed_timestamps: bool,
    // From the QLOGDIR environment variable, via clap (Args.qlog_dir). Read
    // by `setup_qlog`, which `EndpointConfig::setup_qlog` is now generic
    // over `AppData` for — see quiche_endpoint's `Endpoint::connect`/accept
    // path, which passes `&self.app_data` through to it.
    qlog_dir: Option<std::path::PathBuf>,
    /// `--qlog-level`: gates quiche's own `quic:*` events (see `setup_qlog`).
    qlog_level: crate::QlogLevelArg,
    /// `--reconnect`: args and local address to re-dial upstreams with.
    reconnect: Option<(Args, std::net::SocketAddr)>,
    /// `--reconnect`: upstream URLs to re-dial, and when.
    pending_reconnects: Vec<(Instant, String)>,
    #[cfg(test)]
    status: std::sync::Arc<std::sync::Mutex<NodeStatus>>,
}

/// What tests wait for instead of sleeping, updated every tick.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct NodeStatus {
    /// Tracks with an accepted upstream subscription.
    pub(crate) subscribed: HashSet<NamespaceTrackname>,
}

#[cfg(test)]
fn publish_status(ad: &AppData) {
    let subscribed = ad.local_tracks.iter()
        .filter(|(_, track)| track.subscriptions.iter().any(|s| s.track_alias.is_some()))
        .map(|(nt, _)| nt.clone())
        .collect();
    ad.status.lock().unwrap().subscribed = subscribed;
}

impl AppData {
    fn new_local_track(&self) -> LocalTrack {
        LocalTrack::new(self.cache_depth, self.cache_max_obj, self.cache_avg_obj, None)
    }
}

fn earlier(timeout: Option<Duration>, other: Duration) -> Option<Duration> {
    Some(timeout.map_or(other, |t| t.min(other)))
}

/// One-line shutdown-time summary for a track's `TrackStats` — rough counts
/// (see `LocalTrack::stats`), rates are a simple total/duration average.
fn format_track_stats(nt: &NamespaceTrackname, stats: &TrackStats) -> String {
    let secs = stats.first_at.zip(stats.last_at)
        .map(|(a, b)| b.duration_since(a).as_secs_f64())
        .unwrap_or(0.0);
    let obj_rate = if secs > 0.0 { stats.objects as f64 / secs } else { 0.0 };
    let byte_rate = if secs > 0.0 { stats.bytes as f64 / secs } else { 0.0 };
    format!(
        "stats {nt}: {} objects, {} bytes over {secs:.2}s ({obj_rate:.0} obj/s, {byte_rate:.0} B/s), {} canceled",
        stats.objects, stats.bytes, stats.canceled,
    )
}

fn post_handle_recvs(r: &mut Runner) {
    if r.endpoint.app_data().shutdown_requested {
        return shutdown(r);
    }
    // `set_app_timeout` overwrites, so everything waking the loop is folded into one
    let mut app_timeout = connection_timeouts(r);
    let actions = take_signal_actions(r);
    if actions.contains(&SigAction::Migrate) { migrate_client_connections(r); }
    if actions.contains(&SigAction::Ping) { ping_all(r); }
    let now = Instant::now();
    cleanup_closed_conns(r, now);
    if let Some(next) = redial(r, now) { app_timeout = earlier(app_timeout, next); }
    let race_deadlines = r.endpoint.app_data().local_tracks.values()
        .filter_map(|t| t.race.as_ref()?.active.as_ref()?.deadline);
    let fill_deadlines = r.endpoint.app_data().local_tracks.values()
        .flat_map(|t| t.fill.fetches.iter().map(|f| f.deadline));
    for deadline in race_deadlines.chain(fill_deadlines) {
        app_timeout = earlier(app_timeout, deadline.saturating_duration_since(now));
    }

    tick_connections(r);
    // after the tick: its announcements and PUBLISHes are sources, its SUBSCRIBEs may wait
    if let Some(next) = drive_rendezvous(r, now) { app_timeout = earlier(app_timeout, next); }
    if let Some(t) = app_timeout { r.set_app_timeout(t); }
    announce_namespaces(r);
    sync_namespace_subscribers(r);
    sync_tracks_subscribers(r);
    send_active_publishes(r);
    sync_relayed_forward(r);
    drive_tracks(r);
    drive_fetches(r);
    drive_races(r, actions.contains(&SigAction::RaceProbe));
    finalize_publish_done(r);
    if actions.contains(&SigAction::TogglePaddingStream) { toggle_padding_streams(r); }
    if actions.contains(&SigAction::Unpub) {
        let targets = r.endpoint.app_data().unpub_namespaces.clone();
        withdraw_namespaces(r, &targets);
    }
    for_each_moq_handle(&mut r.endpoint, |moq, _| {
        moq.session.send_padding(&mut moq.t);
    });
    #[cfg(test)]
    publish_status(r.endpoint.app_data());
}

/// Pulls objects in from each track's subscriptions into its cache and
/// forwards them to its subscribers, until nothing progresses: a backlog
/// drains within the tick.
fn drive_tracks(r: &mut Runner) {
    loop {
        let (conns, ad) = r.endpoint.mut_conns_and_app_data();
        let mut any_progress = false;
        for (nt, track) in ad.local_tracks.iter_mut() {
            let (progress, race_winner) = drive_track_subscriptions(nt, track, conns);
            any_progress |= progress | drive_cache_fill(nt, track, conns);
            if let Some(winner) = race_winner { resolve_race(nt, track, winner, conns); }
            drive_pending_subscribes(nt, track, conns);
            let cache = &track.cache;
            for out in &mut track.file_outputs { out.drain(cache); }
        }
        // one egress pass across all tracks: priority orders aliases across tracks
        any_progress |= forward_local_tracks(&mut ad.local_tracks, conns);
        if !any_progress { break; }
    }
}

/// `#race` bookkeeping, driven by its own I/O: once per tick, not in the `drive_tracks` loop.
fn drive_races(r: &mut Runner, race_probe: bool) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (nt, track) in ad.local_tracks.iter_mut() {
        replace_lost_leader(nt, track, conns);
        ensure_race_candidates_established(nt, track, conns);
        if race_probe && track.race.is_some() { trigger_race(nt, track, conns); }
        poll_active_race(nt, track, conns);
    }
}

/// run f for every moq_handle
fn for_each_moq_handle<F>(e: &mut Endpoint<ConnAppData, AppData>, mut f: F)
where 
    F: FnMut(&mut MoqHandle, &mut AppData)
{
    let (conns, app_data) = e.mut_conns_and_app_data();
    for (_, conn) in conns {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        f(&mut moq, app_data);
    }
     
}

/// Looks up `cid`'s MoQ session handle, if the connection still exists and
/// has completed its WebTransport/MoQ session setup. The single-call
/// replacement for the `conns.get_mut(cid)` + `moq_helper.moq_handle(...)`
/// pair repeated throughout the relay/forwarding code below.
pub(crate) fn moq_handle_for(conns: &mut ConnMap<ConnAppData>, cid: ClientId) -> Option<MoqHandle<'_>> {
    let conn = conns.get_mut(cid)?;
    conn.app_data.moq_helper.moq_handle(&mut conn.conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_location_is_first_incomplete_object() {
        let mut track = LocalTrack::new(8, 64, 16, None);
        assert_eq!(track.resume_location(), None);

        let location = |object| Location { group: 0, object };
        for object in 0..3 {
            track.cache.begin(NewObject::new(location(object), 4)).unwrap();
        }
        fold(&mut track.cache, location(0), 4, b"full");
        fold(&mut track.cache, location(1), 2, b"pa");
        fold(&mut track.cache, location(2), 4, b"full");
        assert_eq!(track.resume_location(), Some(location(1)));

        fold(&mut track.cache, location(1), 4, b"rt");
        assert_eq!(track.resume_location(), Some(location(3)));
    }

    #[test]
    fn filter_start_is_relative_to_largest() {
        let start = |filter, largest| FilterRange::resolve(filter, largest).start;
        let largest = Some(Location { group: 2, object: 1 });
        assert_eq!(start(LocationFilter::LargestObject, largest), Some(Location { group: 2, object: 2 }));
        assert_eq!(start(LocationFilter::NextGroupStart, largest), Some(Location { group: 3, object: 0 }));
        assert_eq!(start(LocationFilter::LargestObject, None), None);
        let absolute = Location { group: 1, object: 5 };
        assert_eq!(start(LocationFilter::AbsoluteStart { start: absolute }, None), Some(absolute));
    }

    #[test]
    fn absolute_range_ends_after_end_group() {
        let range = FilterRange::resolve(LocationFilter::AbsoluteRange { start: Location { group: 1, object: 5 }, end_group: 2 }, None);
        assert!(!range.contains(Location { group: 1, object: 4 }));
        assert!(range.contains(Location { group: 2, object: 100 }));
        assert!(!range.contains(Location { group: 3, object: 0 }));
    }

    #[test]
    fn abandoned_object_survives_while_another_source_fills_it() {
        let mut track = LocalTrack::new(8, 64, 16, None);
        let shared = Location { group: 0, object: 0 };
        let done = Location { group: 0, object: 1 };
        for loc in [shared, done] {
            track.cache.begin(NewObject::new(loc, 4)).unwrap();
        }
        fold(&mut track.cache, done, 4, b"full");
        let mut other = TrackSubscription::new(ClientId::default(), false, SubscribeOptions::default());
        other.in_progress.push((shared, 4));
        track.subscriptions.push(other);

        cancel_abandoned(&mut track.cache, &track.subscriptions, &mut track.stats, shared);
        cancel_abandoned(&mut track.cache, &track.subscriptions, &mut track.stats, done);
        assert_eq!(track.cache.find(shared).unwrap().state(), moq_object_cache::ObjectState::Progressing);
        assert_eq!(track.cache.find(done).unwrap().state(), moq_object_cache::ObjectState::Completed);

        track.subscriptions.clear();
        cancel_abandoned(&mut track.cache, &track.subscriptions, &mut track.stats, shared);
        assert_eq!(track.cache.find(shared).unwrap().state(), moq_object_cache::ObjectState::Canceled);
        assert_eq!(track.stats.canceled, 1);
    }

    #[test]
    fn format_track_stats_reports_counts_and_rates() {
        let nt: NamespaceTrackname = "track0--data".parse().unwrap();
        let start = Instant::now();
        let stats = TrackStats {
            objects: 200,
            bytes: 2_000_000,
            first_at: Some(start),
            last_at: Some(start + Duration::from_secs(2)),
            canceled: 3,
        };
        let line = format_track_stats(&nt, &stats);
        assert!(line.contains("track0--data"));
        assert!(line.contains("200 objects"));
        assert!(line.contains("2000000 bytes"));
        assert!(line.contains("100 obj/s"));
        assert!(line.contains("1000000 B/s"));
        assert!(line.contains("3 canceled"));
    }

    /// `--reconnect` retries an upstream that doesn't resolve instead of panicking.
    #[test]
    fn unresolvable_peer_is_an_error() {
        let local: std::net::SocketAddr = "0.0.0.0:0".parse().unwrap();
        assert_eq!(resolve_peer("https://127.0.0.1:4443/moq", local).unwrap().1, "127.0.0.1:4443".parse().unwrap());
        assert!(resolve_peer("https://upstream.invalid:4443/moq", local).is_err());
        assert!(resolve_peer("not a url", local).is_err());
    }

    #[test]
    fn format_track_stats_no_objects_is_zero_not_nan() {
        let nt: NamespaceTrackname = "track0--data".parse().unwrap();
        let line = format_track_stats(&nt, &TrackStats::default());
        assert!(line.contains("0 objects, 0 bytes over 0.00s"));
        assert!(line.contains("0 obj/s, 0 B/s"));
        assert!(line.contains("0 canceled"));
    }
}

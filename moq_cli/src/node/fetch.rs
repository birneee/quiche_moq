//! A subscriber's FETCH (draft-ietf-moq-transport-18 §10.12): served from the cache when it holds
//! the range's start, else relayed to a publisher of the track and its objects passed through.

use super::*;
use std::collections::VecDeque;
use moq_object_cache::ObjectState;
use quiche_moq::request::Fetch;
use quiche_moq::wire::control_message::{FetchKind, FetchRange};
use quiche_moq::wire::fetch::{FetchEntry, FetchObject};
use quiche_moq::wire::object::ObjectStatus;
use quiche_moq::wire::GroupOrder;
use quiche_moq::{FetchOk, FetchOptions, FetchResponse, Fetching, RequestFailure};

/// A subscriber's FETCH on `client_id`, answered across ticks by `drive_fetches`.
pub(super) struct DownstreamFetch {
    client_id: ClientId,
    state: FetchState,
}

impl DownstreamFetch {
    pub(super) fn new(client_id: ClientId, request: Request<Fetch>) -> Self {
        Self { client_id, state: FetchState::New(request) }
    }
}

enum FetchState {
    New(Request<Fetch>),
    /// relayed to `source`, awaiting its FETCH_OK
    Requested { request: Request<Fetch>, source: ClientId, upstream: Fetching },
    FromCache(CacheServing),
    Relaying(Relaying),
}

/// A FETCH relayed from `source`: its fetch stream passed through.
struct Relaying {
    response: FetchResponse,
    source: ClientId,
    upstream: Fetching,
    /// an entry the subscriber's stream had no room for
    entry: Option<FetchEntry>,
    /// payload still to read from `source`
    payload_left: usize,
    /// read payload not yet sent
    carry: Vec<u8>,
}

/// A FETCH served from the cache.
struct CacheServing {
    response: FetchResponse,
    nt: NamespaceTrackname,
    /// the cached objects of the range, in the fetch's order
    pending: VecDeque<Served>,
    /// the object whose payload is sent, and how much of it was
    sending: Option<(Location, usize)>,
    /// whether unknown ranges can be told: ascending, draft 16 on
    unknown_ranges: bool,
}

/// What a FETCH served from the cache sends next.
#[derive(Clone, Copy)]
enum Served {
    Object(Location),
    /// The Objects up to this Location, inclusive, are of unknown status (draft-ietf-moq-transport-18 §11.4.4.2).
    Unknown(Location),
}

enum Route {
    /// the joined subscription is still pending
    Wait,
    Cache(NamespaceTrackname, FetchRange),
    Upstream(NamespaceTrackname, FetchRange, ClientId),
    Reject(RequestErrorCode),
}

/// Where `cid`'s FETCH is answered from.
fn route(ad: &AppData, cid: ClientId, kind: &FetchKind) -> Route {
    let (nt, range) = match kind {
        FetchKind::Standalone { namespace_trackname, range } => (namespace_trackname.clone(), *range),
        FetchKind::RelativeJoining { joining_request_id, .. } | FetchKind::AbsoluteJoining { joining_request_id, .. } => {
            let joined = ad.local_tracks.iter()
                .find_map(|(nt, track)| track.aliases.iter().find(|a| a.conn_id == cid && a.request_id == *joining_request_id).map(|a| (nt, a)));
            let Some((nt, alias)) = joined else {
                // §10.12.2: a Joining FETCH waits for a Pending subscription
                let pending = ad.local_tracks.values().any(|t| t.pending_subscribes.iter().any(|p| p.client_id == cid && p.request.request_id() == *joining_request_id))
                    || ad.rendezvous_holds(cid, *joining_request_id);
                return if pending { Route::Wait } else { Route::Reject(RequestErrorCode::InvalidJoiningRequestId) };
            };
            // §10.12.2: only joining a forwarding subscription with published objects
            let Some(joining) = alias.joining_location.filter(|_| alias.forward) else { return Route::Reject(RequestErrorCode::InvalidRange) };
            let start_group = match kind {
                FetchKind::RelativeJoining { groups_before, .. } => joining.group.saturating_sub(*groups_before),
                FetchKind::AbsoluteJoining { start_group, .. } => *start_group,
                FetchKind::Standalone { .. } => unreachable!(),
            };
            // §10.12.2.1: contiguous with the subscription, up to its Joining Location
            (nt.clone(), FetchRange { start: Location { group: start_group, object: 0 }, end: joining.next() })
        }
    };
    let track = ad.local_tracks.get(&nt);
    let cached_from = track.and_then(|t| t.cache.next_after(None)).map(|o| o.location());
    if cached_from.is_some_and(|oldest| oldest <= range.start) {
        return Route::Cache(nt, range);
    }
    let source = track.and_then(|t| t.subscriptions.iter().find(|s| s.track_alias.is_some()).map(|s| s.client_id))
        .or_else(|| ad.sub_passive_wildcards.iter().any(|p| p.matches(&nt)).then(|| pick_one(ad, nt.namespace())).flatten());
    match (source, track) {
        (Some(source), _) => Route::Upstream(nt, range, source),
        // the objects before the cache are of unknown status, see `start_fetch`
        (None, Some(_)) => Route::Cache(nt, range),
        (None, None) => Route::Reject(RequestErrorCode::DoesNotExist),
    }
}

/// Moves every downstream FETCH on as far as the cache, the publishers and the streams allow.
pub(super) fn drive_fetches(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for fetch in std::mem::take(&mut ad.fetches) {
        if let Some(state) = drive_fetch(fetch.client_id, fetch.state, conns, ad) {
            ad.fetches.push(DownstreamFetch { client_id: fetch.client_id, state });
        }
    }
}

/// The fetch's next state, `None` once it ended.
fn drive_fetch(cid: ClientId, state: FetchState, conns: &mut ConnMap<ConnAppData>, ad: &mut AppData) -> Option<FetchState> {
    match state {
        FetchState::New(request) => start_fetch(cid, request, conns, ad),
        FetchState::Requested { request, source, upstream } => {
            let answer = responses_for(conns, source)?.take_fetch(upstream.request_id());
            let Some(answer) = answer else { return Some(FetchState::Requested { request, source, upstream }) };
            let (mut moq, _) = conn_for(conns, cid)?;
            match answer {
                Ok(ok) => {
                    let response = moq.session.accept_request(request, ok, &mut moq.t).ok()?;
                    Some(FetchState::Relaying(Relaying { response, source, upstream, entry: None, payload_left: 0, carry: Vec::new() }))
                }
                Err(failure) => {
                    let code = match failure {
                        RequestFailure::Rejected(error) => error.error_code(),
                        RequestFailure::Cancelled => RequestErrorCode::InternalError,
                    };
                    moq.session.reject_request(request, code, &mut moq.t).ok();
                    None
                }
            }
        }
        FetchState::FromCache(serving) => serve_from_cache(cid, serving, conns, ad),
        FetchState::Relaying(relaying) => relay(cid, relaying, conns),
    }
}

fn start_fetch(cid: ClientId, request: Request<Fetch>, conns: &mut ConnMap<ConnAppData>, ad: &mut AppData) -> Option<FetchState> {
    let message = request.message();
    let (priority, group_order) = (message.subscriber_priority, message.group_order.unwrap_or(GroupOrder::Ascending));
    match route(ad, cid, &message.kind) {
        Route::Wait => Some(FetchState::New(request)),
        Route::Reject(code) => {
            let (mut moq, _) = conn_for(conns, cid)?;
            info!("conn {cid} reject fetch: {code:?}");
            moq.session.reject_request(request, code, &mut moq.t).ok();
            None
        }
        Route::Cache(nt, range) => {
            let track = &ad.local_tracks[&nt];
            let (mut moq, _) = conn_for(conns, cid)?;
            // §10.12.2 and §10.13: an End Location before the start is an error
            let Some(largest) = track.largest().filter(|&largest| largest >= range.start) else {
                info!("conn {cid} reject fetch of {}: nothing published from {:?}", nt, range.start);
                moq.session.reject_request(request, RequestErrorCode::InvalidRange, &mut moq.t).ok();
                return None;
            };
            let mut objects: Vec<Location> = track.cache.objects_after(None)
                .filter(|o| range.contains(o.location()) && o.status() == ObjectStatus::Normal && o.state() != ObjectState::Canceled)
                .map(|o| o.location())
                .collect();
            let count = objects.len();
            if group_order == GroupOrder::Descending {
                objects.sort_by_key(|l| (std::cmp::Reverse(l.group), l.object));
            }
            // TODO also for a descending FETCH
            let unknown_ranges = group_order == GroupOrder::Ascending && moq.session.version().is_some_and(|v| v.draft() >= 16);
            // §10.12: before the cache, whether objects exist is unknown
            let gap = track.cache.next_after(None).map(|o| o.location())
                .filter(|&oldest| unknown_ranges && oldest > range.start)
                .and_then(Location::prev)
                .map(Served::Unknown);
            let pending: VecDeque<Served> = gap.into_iter().chain(objects.into_iter().map(Served::Object)).collect();
            let ok = FetchOk { end_of_track: false, end_location: range.end_location(largest) };
            let response = moq.session.accept_request(request, ok, &mut moq.t).ok()?;
            info!("conn {cid} fetch of {} from the cache: {count} objects", nt);
            Some(FetchState::FromCache(CacheServing { response, nt, pending, sending: None, unknown_ranges }))
        }
        Route::Upstream(nt, range, source) => {
            let (mut moq, _) = conn_for(conns, source)?;
            let kind = FetchKind::Standalone { namespace_trackname: nt.clone(), range };
            match moq.session.fetch(kind, FetchOptions { priority, group_order }, &mut moq.t) {
                Ok(upstream) => {
                    info!("conn {cid} fetch of {} relayed to conn {source}", nt);
                    Some(FetchState::Requested { request, source, upstream })
                }
                // e.g. out of Request IDs for now
                Err(e) => {
                    info!("conn {cid} fetch of {} can't be relayed: {e:?}", nt);
                    let (mut moq, _) = conn_for(conns, cid)?;
                    moq.session.reject_request(request, RequestErrorCode::InternalError, &mut moq.t).ok();
                    None
                }
            }
        }
    }
}

/// Writes cached objects to the fetch stream as far as the stream takes them.
/// - an object still arriving is written as it arrives
/// - evicted before it was begun: an unknown range if `pending` has them, else left out
/// - evicted or cancelled mid-payload: the stream is reset, by dropping `response`
fn serve_from_cache(
    cid: ClientId, serving: CacheServing,
    conns: &mut ConnMap<ConnAppData>, ad: &AppData,
) -> Option<FetchState> {
    let CacheServing { response, nt, mut pending, mut sending, unknown_ranges } = serving;
    let (mut moq, _) = conn_for(conns, cid)?;
    let cache = &ad.local_tracks.get(&nt)?.cache;
    loop {
        if let Some((loc, sent)) = sending {
            let object = cache.find(loc).filter(|o| o.state() != ObjectState::Canceled)?;
            if sent == object.payload_len() {
                sending = None;
                continue;
            }
            let readable = object.readable_from(sent);
            if readable.is_empty() { break; }
            match moq.session.send_fetch_pld(&response, readable, &mut moq.t) {
                Ok(n) => sending = Some((loc, sent + n)),
                Err(quiche_moq::Error::Done) => break,
                // the subscriber cancelled
                Err(_) => return None,
            }
        } else if let Some(&next) = pending.front() {
            let (entry, object) = match next {
                Served::Unknown(location) => (FetchEntry::EndOfRange { location, unknown: true }, None),
                Served::Object(loc) => match cache.find(loc) {
                    Some(object) => (FetchEntry::Object(FetchObject {
                        location: loc,
                        subgroup_id: (!object.datagram()).then(|| object.subgroup_id()),
                        publisher_priority: object.publisher_priority(),
                        properties: object.properties().clone(),
                        payload_len: object.payload_len(),
                    }), Some(loc)),
                    None => {
                        pending.pop_front();
                        if unknown_ranges { pending.push_front(Served::Unknown(loc)); }
                        continue;
                    }
                },
            };
            match moq.session.send_fetch_entry(&response, &entry, &mut moq.t) {
                Ok(()) => {
                    pending.pop_front();
                    sending = object.map(|loc| (loc, 0));
                }
                Err(quiche_moq::Error::Done) => break,
                Err(_) => return None,
            }
        } else {
            moq.session.finish_fetch(response, &mut moq.t).ok();
            return None;
        }
    }
    Some(FetchState::FromCache(CacheServing { response, nt, pending, sending, unknown_ranges }))
}

/// Passes `source`'s fetch stream through to the subscriber's as far as both allow.
fn relay(cid: ClientId, mut r: Relaying, conns: &mut ConnMap<ConnAppData>) -> Option<FetchState> {
    let mut buf = [0u8; RECV_CHUNK];
    loop {
        if !r.carry.is_empty() {
            let (mut moq, _) = conn_for(conns, cid)?;
            match moq.session.send_fetch_pld(&r.response, &r.carry, &mut moq.t) {
                Ok(n) => { r.carry.drain(..n); }
                Err(quiche_moq::Error::Done) => break,
                Err(_) => return None,
            }
        } else if let Some(next) = r.entry.take() {
            let (mut moq, _) = conn_for(conns, cid)?;
            match moq.session.send_fetch_entry(&r.response, &next, &mut moq.t) {
                Ok(()) => if let FetchEntry::Object(object) = &next { r.payload_left = object.payload_len },
                Err(quiche_moq::Error::Done) => { r.entry = Some(next); break; }
                Err(_) => return None,
            }
        } else if r.payload_left > 0 {
            let (mut moq, _) = conn_for(conns, r.source)?;
            let len = r.payload_left.min(buf.len());
            match moq.session.recv_fetch_pld(&r.upstream, &mut buf[..len], &mut moq.t) {
                Ok(n) => {
                    r.payload_left -= n;
                    r.carry.extend_from_slice(&buf[..n]);
                }
                Err(quiche_moq::Error::Done) => break,
                // the publisher's stream broke off mid-object: the subscriber's is reset
                Err(_) => return None,
            }
        } else {
            let (mut moq, _) = conn_for(conns, r.source)?;
            match moq.session.recv_fetch_entry(&r.upstream, &mut moq.t) {
                Ok(next) => r.entry = Some(next),
                Err(quiche_moq::Error::Done) => break,
                Err(quiche_moq::Error::Fin) => {
                    let (mut moq, _) = conn_for(conns, cid)?;
                    moq.session.finish_fetch(r.response, &mut moq.t).ok();
                    return None;
                }
                Err(_) => return None,
            }
        }
    }
    Some(FetchState::Relaying(r))
}

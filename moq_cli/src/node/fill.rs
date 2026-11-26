//! Objects a subscriber wants from before the upstream subscription's start, fetched upstream into
//! the cache and forwarded from there (draft-ietf-moq-transport-19 §2.3.1).

use super::*;
use quiche_moq::wire::control_message::{FetchKind, FetchRange};
use quiche_moq::wire::fetch::{FetchEntry, FetchObject};
use quiche_moq::wire::GroupOrder;
use quiche_moq::FetchOptions;
use crate::local_track::UpstreamFill;

/// How long a fill may take, PUBLISH_DONE waits for it. Dropping it cancels its FETCH.
pub(super) const FILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts an upstream FETCH for what subscribers want before the track's objects, and reads the
/// running ones into the cache, giving up after `FILL_TIMEOUT`. Whether anything was read.
pub(super) fn drive_cache_fill(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) -> bool {
    start_fill(nt, track, conns);
    let LocalTrack { cache, subscriptions, aliases, stats, fill, .. } = track;
    let mut progress = false;
    let now = Instant::now();
    fill.fetches.retain_mut(|f| {
        let (read, running) = read_fill(f, cache, aliases, conns);
        progress |= read;
        let running = running && now < f.deadline;
        if !running {
            if let Some((loc, _, _)) = f.reading.take() {
                cancel_abandoned(cache, subscriptions, stats, loc);
            }
            info!("conn {} fill of {nt} up to {:?} done", f.source, f.until);
        }
        running
    });
    progress
}

/// FETCHes from the earliest wanted Location up to what is fetched or subscribed already.
fn start_fill(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    let LocalTrack { subscriptions, fill, .. } = track;
    let Some(wanted) = fill.wanted_from else { return };
    let Some((source, subscribed_from)) = subscriptions.iter().find_map(|s| Some((s.client_id, s.start?))) else { return };
    let until = fill.fetched_from.map_or(subscribed_from, |fetched| fetched.min(subscribed_from));
    let Some(end) = end_before(until).filter(|_| wanted < until) else { return };
    let Some((mut moq, _)) = conn_for(conns, source) else { return };
    let kind = FetchKind::Standalone { namespace_trackname: nt.clone(), range: FetchRange { start: wanted, end } };
    match moq.session.fetch(kind, FetchOptions { priority: DEFAULT_SUBSCRIBER_PRIORITY, group_order: GroupOrder::Ascending }, &mut moq.t) {
        Ok(upstream) => {
            info!("conn {source} fill of {nt} from {wanted:?} up to {until:?}");
            fill.fetched_from = Some(wanted);
            fill.fetches.push(UpstreamFill { source, upstream, until, accepted: false, deadline: Instant::now() + FILL_TIMEOUT, reading: None, last_of_subgroup: HashMap::new() });
        }
        Err(quiche_moq::Error::Unimplemented) => {
            info!("conn {source} fill of {nt} from {wanted:?}: no FETCH in this draft");
            fill.fetched_from = Some(wanted);
        }
        // e.g. out of Request IDs for now, retried next tick
        Err(e) => info!("conn {source} fill of {nt} not fetched: {e:?}"),
    }
}

/// A FETCH's End Location for the Objects before `until` (draft-ietf-moq-transport-18 §10.12.1),
/// `None` if there are none.
fn end_before(until: Location) -> Option<Location> {
    match until {
        Location { group: 0, object: 0 } => None,
        Location { group, object: 0 } => Some(Location { group: group - 1, object: 0 }),
        partial => Some(partial),
    }
}

/// Reads `fill`'s objects into the cache as far as they arrived: whether anything was read, and
/// whether it is still running.
fn read_fill(
    fill: &mut UpstreamFill, cache: &mut PackedObjectCache, aliases: &mut [LocalAliasState], conns: &mut ConnMap<ConnAppData>,
) -> (bool, bool) {
    if !fill.accepted {
        let Some(responses) = responses_for(conns, fill.source) else { return (false, false) };
        match responses.take_fetch(fill.upstream.request_id()) {
            None => return (false, true),
            Some(Ok(_)) => fill.accepted = true,
            Some(Err(e)) => {
                info!("conn {} fill up to {:?} failed: {e:?}", fill.source, fill.until);
                return (false, false);
            }
        }
    }
    let Some(mut moq) = moq_handle_for(conns, fill.source) else { return (false, false) };
    let mut read = false;
    let mut buf = [0u8; RECV_CHUNK];
    loop {
        if let Some((loc, payload_len, received)) = fill.reading {
            if received >= payload_len {
                fill.reading = None;
                continue;
            }
            let chunk = &mut buf[..(payload_len - received).min(RECV_CHUNK)];
            match moq.session.recv_fetch_pld(&fill.upstream, chunk, &mut moq.t) {
                Ok(n) => {
                    fold(cache, loc, received + n, &chunk[..n]);
                    fill.reading = Some((loc, payload_len, received + n));
                    read = true;
                }
                Err(quiche_moq::Error::Done) => return (read, true),
                Err(_) => return (read, false),
            }
            continue;
        }
        match moq.session.recv_fetch_entry(&fill.upstream, &mut moq.t) {
            Ok(FetchEntry::Object(FetchObject { location: loc, subgroup_id, publisher_priority, properties, payload_len })) => {
                let (datagram, subgroup_id) = (subgroup_id.is_none(), subgroup_id.unwrap_or(0));
                match begin_or_join(cache, NewObject { location: loc, subgroup_id, publisher_priority, end_of_subgroup: false, properties, payload_len }) {
                    Ok(evicted) => {
                        cache_to_moq::mark_evicted(aliases.iter_mut().map(|a| &mut a.state), &evicted);
                        if datagram
                            && let Some(mut cached) = cache.find_mut(loc)
                        {
                            cached.set_datagram();
                        }
                    }
                    // nothing to read it into: its payload is read and dropped by `fold`
                    Err(JoinError::CacheFull | JoinError::Mismatch) => {}
                }
                let last = fill.last_of_subgroup.entry((loc.group, subgroup_id)).or_insert(loc);
                *last = (*last).max(loc);
                fill.reading = Some((loc, payload_len, 0));
                read = true;
            }
            Ok(FetchEntry::EndOfRange { .. }) => {}
            Err(quiche_moq::Error::Done) => return (read, true),
            Err(quiche_moq::Error::Fin) => {
                // the Subgroups of Groups before `until` end with what was fetched of them
                for (&(group, _), &loc) in &fill.last_of_subgroup {
                    if group < fill.until.group {
                        cache.mark_end_of_subgroup(loc);
                    }
                }
                return (true, false);
            }
            Err(_) => return (read, false),
        }
    }
}

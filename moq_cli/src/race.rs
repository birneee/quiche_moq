//! `#race` leader election: establishing candidates, triggering a race
//! (`SigAction::RaceProbe`), and resolving it to a winner. See `LocalTrack::race`.

use std::collections::HashMap;
use quiche_moq::SubscribeOptions;
use std::time::{Duration, Instant};

use log::{info, warn};

use quiche_mio_runner::quiche_endpoint::ClientId;
use quiche_mio_runner::quiche_endpoint::ConnMap;
use quiche_moq::wire::control_message::subscribe::LocationFilter;
use quiche_moq::wire::{Location, NamespaceTrackname, RequestId, TrackAlias};

use crate::args::RaceMode;
use crate::conn::ConnAppData;
use crate::local_track::{cancel_abandoned, ActiveRace, LocalTrack, RaceCandidate, TrackSubscription};
use crate::node::{conn_for, moq_handle_for, responses_for};

/// Time a race waits for a winner, the current leader keeps the track then.
const RACE_TIMEOUT: Duration = Duration::from_secs(5);
/// Delay before a failed settle is tried again.
const SETTLE_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Ensures every known `#race` candidate has a persistent, established
/// Forward=0 subscription (draft-ietf-moq-transport-19 §5.1: kept alive so Forward=1
/// takes effect immediately when `trigger_race` needs it — no fresh SUBSCRIBE
/// round trip mid-race), and resolves pending initial SUBSCRIBE_OKs.
pub(crate) fn ensure_race_candidates_established(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    let Some(race) = track.race.as_mut() else { return };
    for &cid in &race.candidates {
        if race.established.contains_key(&cid) { continue }
        let Some((mut moq, handles)) = conn_for(conns, cid) else { continue };
        if let Ok(subscription) = moq.session.subscribe(nt, race.standby_options(), &mut moq.t) {
            race.established.insert(cid, RaceCandidate { request_id: subscription.request_id(), track_alias: None });
            handles.subscriptions.insert(subscription.request_id(), subscription);
        }
    }

    let awaiting_initial: Vec<ClientId> = race.established.iter()
        .filter(|(_, c)| c.track_alias.is_none())
        .map(|(&cid, _)| cid)
        .collect();
    for cid in awaiting_initial {
        let rid = race.established[&cid].request_id;
        let Some(responses) = responses_for(conns, cid) else { race.established.remove(&cid); continue };
        match responses.take_subscribe(rid) {
            Some(Ok((alias, _))) => { race.established.get_mut(&cid).unwrap().track_alias = Some(alias); }
            Some(Err(_)) => { race.established.remove(&cid); }
            None => {} // still waiting
        }
    }
}

/// `SigAction::RaceProbe`: fires a `#race` track's leader election, see
/// `trigger_data_race` and `trigger_probe_race`.
pub(crate) fn trigger_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    match track.race.as_ref().map(|r| r.mode) {
        Some(RaceMode::Data) => trigger_data_race(nt, track, conns),
        Some(mode) => trigger_probe_race(nt, track, conns, mode),
        None => {}
    }
}

/// Where a new forwarder takes over from the leader: the first object the
/// leader is still receiving, so a handover loses nothing partially received,
/// else right after the cache; `{0, 0}` without content.
fn race_start(track: &LocalTrack) -> Location {
    let leader_partial = track.subscriptions.iter()
        .filter(|s| s.race)
        .flat_map(|s| s.in_progress.iter().map(|&(loc, _)| loc))
        .min();
    leader_partial.or_else(|| track.next_location()).unwrap_or(Location { group: 0, object: 0 })
}

/// Candidate subscription switched to Forward=1, materialized so
/// `drive_track_subscriptions` reads its objects.
/// - already subscribed: priority and delivery timeout aren't needed here
fn forwarding_subscription(cid: ClientId, rid: RequestId, alias: Option<TrackAlias>, start: Location) -> TrackSubscription {
    let mut sub = TrackSubscription::new(cid, false, SubscribeOptions { filter: LocationFilter::AbsoluteStart { start }, ..SubscribeOptions::default() });
    sub.request_id = Some(rid);
    sub.track_alias = alias;
    sub
}

/// `RaceMode::Data`: every established, non-leader candidate forwards from
/// `race_start`; the first with content there wins.
/// - no-op if a race is already in flight, or nothing established to race
fn trigger_data_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    let target = race_start(track);
    let LocalTrack { subscriptions, race, .. } = track;
    let Some(race) = race.as_mut() else { return };
    if race.active.is_some() { return }
    let leader_cid = subscriptions.iter().find(|s| s.race).map(|s| s.client_id);

    let mut participants = HashMap::new();
    for &cid in &race.candidates {
        if Some(cid) == leader_cid { continue }
        let Some(candidate) = race.established.get(&cid) else { continue };
        let Some(track_alias) = candidate.track_alias else { continue }; // still awaiting its own SUBSCRIBE_OK
        let rid = candidate.request_id;
        let Some(update_rid) = request_update(conns, cid, rid, true, Some(LocationFilter::AbsoluteStart { start: target })) else { continue };
        subscriptions.push(forwarding_subscription(cid, rid, Some(track_alias), target));
        participants.insert(cid, update_rid);
    }
    if participants.is_empty() { return }
    info!("race: triggered for {nt} — {} candidate(s) racing against {target:?}", participants.len());
    let deadline = Some(Instant::now() + RACE_TIMEOUT);
    race.active = Some(ActiveRace { target, participants, forwarder: None, deadline });
}

/// Checks every pending `REQUEST_UPDATE_OK`/`REQUEST_ERROR` for the
/// track's in-flight race, if any — see `trigger_race` for how a race
/// resolves.
/// - `LARGEST_OBJECT >= target`: wins immediately, no need to wait for the Object
/// - not ready (smaller `LARGEST_OBJECT`, no location, or `REQUEST_ERROR`):
///   demotes just that candidate, race stays open for the rest
pub(crate) fn poll_active_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    match track.race.as_ref().map(|r| r.mode) {
        Some(RaceMode::Data) => poll_data_race(nt, track, conns),
        Some(_) => poll_probe_race(nt, track, conns),
        None => {}
    }
}

/// REQUEST_UPDATE responses of a race's participants, see `poll_participants`.
#[derive(Default)]
struct Polled {
    /// first caught up (`LARGEST_OBJECT >= target`)
    winner: Option<ClientId>,
    /// answered, but behind
    behind: Vec<ClientId>,
    /// rejected the update or disconnected: its Forward state is unknown
    failed: Vec<ClientId>,
}

impl Polled {
    fn not_ready(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.behind.iter().chain(&self.failed).copied()
    }
}

/// Polls every race participant's REQUEST_UPDATE response, up to the first
/// caught up one.
fn poll_participants(track: &LocalTrack, conns: &mut ConnMap<ConnAppData>) -> Polled {
    let mut polled = Polled::default();
    let Some(active) = track.race.as_ref().and_then(|r| r.active.as_ref()) else { return polled };
    for (&cid, &rid) in &active.participants {
        let Some(responses) = responses_for(conns, cid) else { polled.failed.push(cid); continue };
        match responses.take_request_update(rid) {
            None => {} // still waiting
            Some(Ok(Some(loc))) if loc >= active.target => {
                polled.winner = Some(cid);
                break;
            }
            Some(Ok(_)) => polled.behind.push(cid),
            Some(Err(_)) => polled.failed.push(cid),
        }
    }
    polled
}

fn poll_data_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    let polled = poll_participants(track, conns);
    for cid in polled.not_ready() { demote_race_participant(track, cid, conns); }
    if let Some(winner) = polled.winner {
        resolve_race(nt, track, winner, conns);
        return;
    }
    let Some(race) = track.race.as_mut() else { return };
    if race.active.as_ref().and_then(|a| a.deadline).is_some_and(|deadline| Instant::now() >= deadline) {
        warn!("race: {nt} timed out, the leader keeps the track");
        let active = race.active.take();
        for cid in active.into_iter().flat_map(|a| a.participants.into_keys()) {
            demote_race_participant(track, cid, conns);
        }
    }
}

/// Drops `cid` from the track's in-flight race without deciding a winner:
/// flips it back to Forward=0 and, if that reached it, keeps it in
/// `race.established` as an idle candidate, ready for the next
/// `trigger_race` or `replace_lost_leader`.
pub(crate) fn demote_race_participant(track: &mut LocalTrack, cid: ClientId, conns: &mut ConnMap<ConnAppData>) {
    if let Some(race) = track.race.as_mut()
        && let Some(active) = race.active.as_mut()
    {
        active.participants.remove(&cid);
    }
    let Some(idx) = track.subscriptions.iter().position(|s| s.client_id == cid && !s.race) else { return };
    let sub = track.subscriptions.remove(idx);
    for &(loc, _) in &sub.in_progress {
        cancel_abandoned(&mut track.cache, &track.subscriptions, &mut track.stats, loc);
    }
    // never subscribed, or unreachable: nothing to keep established
    let Some(request_id) = sub.request_id else { return };
    if request_update(conns, cid, request_id, false, None).is_none() { return }
    let Some(race) = track.race.as_mut() else { return };
    race.established.insert(cid, RaceCandidate { request_id, track_alias: sub.track_alias });
    if !race.candidates.contains(&cid) { race.candidates.push(cid); }
}

/// The leader is gone (disconnected or ended) and no race is running: an
/// established standby takes over at the first incomplete object, without a
/// fresh SUBSCRIBE round trip.
pub(crate) fn replace_lost_leader(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    if track.subscriptions.iter().any(|s| s.race) { return }
    let Some(race) = track.race.as_ref() else { return };
    if race.active.is_some() { return }
    let Some((cid, rid, alias)) = race.candidates.iter()
        .find_map(|&cid| race.established.get(&cid).and_then(|c| c.track_alias.map(|alias| (cid, c.request_id, alias))))
    else { return };
    // the lost leader's partial objects stay incomplete in the cache
    let start = track.resume_location().unwrap_or(Location { group: 0, object: 0 });
    if request_update(conns, cid, rid, true, Some(LocationFilter::AbsoluteStart { start })).is_none() { return }
    let mut sub = forwarding_subscription(cid, rid, Some(alias), start);
    sub.race = true;
    track.subscriptions.push(sub);
    if let Some(race) = track.race.as_mut() { race.promote(cid); }
    info!("race: {nt} lost its leader, conn {cid} takes over at {start:?}");
}

/// Declares `winner_cid` the winner of `nt`'s in-flight race and prunes
/// every other participant — including the incumbent leader, if it lost —
/// back to an idle, Forward=0 established candidate (see
/// `demote_race_participant`). No-op if there's no active race, or
/// `winner_cid` isn't racing (a response/Object arriving after the race
/// already resolved another way).
pub(crate) fn resolve_race(nt: &NamespaceTrackname, track: &mut LocalTrack, winner_cid: ClientId, conns: &mut ConnMap<ConnAppData>) {
    let Some(active) = track.race.as_mut().and_then(|r| r.active.take()) else { return };
    let leader_cid = track.subscriptions.iter().find(|s| s.race).map(|s| s.client_id);
    let winner_is_leader = Some(winner_cid) == leader_cid;
    if !winner_is_leader && !active.participants.contains_key(&winner_cid) {
        if let Some(race) = track.race.as_mut() { race.active = Some(active); } // stale — keep the real race open
        return;
    }

    if !winner_is_leader {
        if let Some(old_idx) = track.subscriptions.iter().position(|s| s.race) { track.subscriptions[old_idx].race = false; }
        if let Some(win_idx) = track.subscriptions.iter().position(|s| s.client_id == winner_cid) { track.subscriptions[win_idx].race = true; }
    }
    if let Some(race) = track.race.as_mut() { race.promote(winner_cid); }

    let mut losers: Vec<ClientId> = active.participants.into_keys().filter(|&c| c != winner_cid).collect();
    if !winner_is_leader && let Some(old_cid) = leader_cid { losers.push(old_cid); }
    for loser_cid in losers {
        demote_race_participant(track, loser_cid, conns);
    }
    info!("race: {nt} settled on conn {winner_cid}{}", if winner_is_leader { " (incumbent leader, unaffected)" } else { "" });
}

/// REQUEST_UPDATE of Forward (and filter) on `cid`'s subscription `rid`;
/// returns the update's Request ID.
fn request_update(conns: &mut ConnMap<ConnAppData>, cid: ClientId, rid: RequestId, forward: bool, filter: Option<LocationFilter>) -> Option<RequestId> {
    let (mut moq, handles) = conn_for(conns, cid)?;
    let subscription = handles.subscriptions.get(&rid)?;
    moq.session.request_update_forward(subscription, forward, filter, &mut moq.t)
        .inspect_err(|e| warn!("race: REQUEST_UPDATE on conn {cid} failed: {e:?}"))
        .ok()
}

/// `cid`'s Request ID and Track Alias, from its `TrackSubscription` or idle `RaceCandidate`.
fn subscription_ids(track: &LocalTrack, cid: ClientId) -> Option<(RequestId, Option<TrackAlias>)> {
    track.subscriptions.iter()
        .find(|s| s.client_id == cid)
        .and_then(|s| s.request_id.map(|rid| (rid, s.track_alias)))
        .or_else(|| track.race.as_ref()?.established.get(&cid).map(|c| (c.request_id, c.track_alias)))
}

/// `RaceMode::Keep`/`Switch`: probes leader and standbys with REQUEST_UPDATE,
/// decided in `poll_probe_race`.
/// - exactly one Forward=1 participant (`Keep`: leader, `Switch`: a standby)
/// - no duplicated load on a shared link, unlike `Data`
fn trigger_probe_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>, mode: RaceMode) {
    let target = race_start(track);
    let LocalTrack { subscriptions, race, .. } = track;
    let Some(race) = race.as_mut() else { return };
    if race.active.is_some() { return }
    let Some(leader) = subscriptions.iter().find(|s| s.race) else { return };
    let Some(leader_rid) = leader.request_id else { return };
    let leader_cid = leader.client_id;
    let standbys: Vec<(ClientId, RequestId, TrackAlias)> = race.candidates.iter()
        .filter(|&&cid| cid != leader_cid)
        .filter_map(|&cid| race.established.get(&cid).and_then(|c| c.track_alias.map(|alias| (cid, c.request_id, alias))))
        .collect();
    let Some(&(first_standby, ..)) = standbys.first() else { return };
    let forwarder = if mode == RaceMode::Keep { leader_cid } else { first_standby };

    let mut participants = HashMap::new();
    if let Some(&(cid, rid, alias)) = standbys.iter().find(|&&(cid, ..)| cid == forwarder) {
        // first: without a forwarding standby the leader must not be paused
        let Some(update) = request_update(conns, cid, rid, true, Some(LocationFilter::AbsoluteStart { start: target })) else {
            warn!("race ({mode:?}): not triggered for {nt}");
            return;
        };
        subscriptions.push(forwarding_subscription(cid, rid, Some(alias), target));
        participants.insert(cid, update);
    }
    // leader probed too: `Keep` no-op Forward=1, `Switch` paused
    let leader_forward = forwarder == leader_cid;
    if let Some(update) = request_update(conns, leader_cid, leader_rid, leader_forward, None) {
        participants.insert(leader_cid, update);
    }
    for (cid, rid, _) in standbys {
        if cid != forwarder
            && let Some(update) = request_update(conns, cid, rid, false, None)
        {
            participants.insert(cid, update);
        }
    }
    info!("race ({mode:?}): triggered for {nt} — {} participant(s) probed, conn {forwarder} forwarding, target {target:?}", participants.len());
    let deadline = Some(Instant::now() + RACE_TIMEOUT);
    race.active = Some(ActiveRace { target, participants, forwarder: Some(forwarder), deadline });
}

/// `RaceMode::Keep`/`Switch`: first REQUEST_UPDATE_OK with LARGEST_OBJECT at
/// or past the target wins.
/// - behind or error: participant leaves, Forward state untouched
/// - no answer until the deadline: participant leaves as well
/// - nobody left: initial forwarder keeps the track
fn poll_probe_race(nt: &NamespaceTrackname, track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    let polled = poll_participants(track, conns);
    let winner = polled.winner;
    let leader = track.subscriptions.iter().find(|s| s.race).map(|s| s.client_id);
    let Some(race) = track.race.as_mut() else { return };
    let Some(active) = race.active.as_mut() else { return };
    for cid in polled.not_ready() { active.participants.remove(&cid); }
    // its Forward=1 may not have taken effect
    if active.forwarder.is_some_and(|f| polled.failed.contains(&f)) { active.forwarder = None; }
    if winner.is_none() && active.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        warn!("race: {nt} timed out, {} participant(s) did not answer", active.participants.len());
        active.participants.clear();
    }
    if winner.is_none() && !active.participants.is_empty() { return }
    // nobody won: the forwarder keeps the track, else the leader resumes
    let fallback = [active.forwarder, leader].into_iter().flatten().find(|&cid| moq_handle_for(conns, cid).is_some());
    match winner.or(fallback) {
        Some(winner) => settle_probe_race(nt, track, winner, conns),
        None => {
            warn!("race: {nt} ended without a reachable forwarder");
            race.active = None;
        }
    }
}

/// Ends a `Keep`/`Switch` race: `winner` forwards, all others go idle (Forward=0).
/// - a new forwarder starts at `race_start`
/// - the forwarder keeps the track if the winner can't be switched to
///   Forward=1, without a forwarder the settle is retried next tick
fn settle_probe_race(nt: &NamespaceTrackname, track: &mut LocalTrack, winner: ClientId, conns: &mut ConnMap<ConnAppData>) {
    let Some(active) = track.race.as_mut().and_then(|r| r.active.take()) else { return };
    let mut winner = winner;
    if active.forwarder != Some(winner) && !switch_to_forwarding(track, winner, conns) {
        let Some(forwarder) = active.forwarder else {
            warn!("race: {nt} could not switch to conn {winner} and has no forwarder, retrying");
            let deadline = Some(Instant::now() + SETTLE_RETRY_INTERVAL);
            if let Some(race) = track.race.as_mut() { race.active = Some(ActiveRace { deadline, ..active }); }
            return;
        };
        winner = forwarder;
    }
    // all forwarding subscriptions but the winner's lose
    for s in track.subscriptions.iter_mut() { s.race = s.client_id == winner; }
    if let Some(race) = track.race.as_mut() { race.promote(winner); }
    let losers: Vec<ClientId> = track.subscriptions.iter().filter(|s| !s.race).map(|s| s.client_id).collect();
    for loser in losers {
        demote_race_participant(track, loser, conns);
    }
    info!("race: {nt} settled on conn {winner}{}", if active.forwarder == Some(winner) { " (kept)" } else { " (switched)" });
}

/// Flips `cid` to Forward=1 from `race_start` and makes sure its objects are
/// read; false if the update couldn't be sent.
fn switch_to_forwarding(track: &mut LocalTrack, cid: ClientId, conns: &mut ConnMap<ConnAppData>) -> bool {
    let start = race_start(track);
    let Some((rid, alias)) = subscription_ids(track, cid) else { return false };
    if request_update(conns, cid, rid, true, Some(LocationFilter::AbsoluteStart { start })).is_none() { return false }
    if !track.subscriptions.iter().any(|s| s.client_id == cid) {
        track.subscriptions.push(forwarding_subscription(cid, rid, alias, start));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_track::RaceState;
    use moq_object_cache::{NewObject, ObjectCache, ObjectState};

    fn race_state(mode: RaceMode, active: Option<ActiveRace>) -> RaceState {
        RaceState { active, mode, ..RaceState::default() }
    }

    /// The loser's partial object is canceled, it would otherwise block
    /// in-order readers until it is evicted.
    #[test]
    fn demoted_participant_cancels_partial_objects() {
        let mut track = LocalTrack::new(8, 64, 16, None);
        let loc = Location { group: 0, object: 0 };
        track.cache.begin(NewObject::new(loc, 4)).unwrap();
        let mut loser = TrackSubscription::new(1, false, SubscribeOptions::default());
        loser.in_progress.push((loc, 4));
        track.subscriptions.push(loser);

        demote_race_participant(&mut track, 1, &mut ConnMap::new());

        assert!(track.subscriptions.is_empty());
        assert_eq!(track.cache.find(loc).map(|o| o.state()), Some(ObjectState::Canceled));
    }

    /// A race whose forwarder is unreachable must not settle on it and
    /// demote the leader.
    #[test]
    fn probe_race_never_settles_on_unreachable_forwarder() {
        let nt: NamespaceTrackname = "ns--track".parse().unwrap();
        let mut track = LocalTrack::new(8, 64, 16, None);
        let mut leader = TrackSubscription::new(1, false, SubscribeOptions::default());
        leader.race = true;
        track.subscriptions.push(leader);
        let active = ActiveRace {
            target: Location { group: 0, object: 0 },
            participants: HashMap::new(),
            forwarder: Some(2),
            deadline: Some(Instant::now() + RACE_TIMEOUT),
        };
        track.race = Some(race_state(RaceMode::Switch, Some(active)));

        poll_active_race(&nt, &mut track, &mut ConnMap::new());

        assert!(track.race.as_ref().unwrap().active.is_none());
        assert!(track.subscriptions.iter().any(|s| s.client_id == 1 && s.race), "leader must stay");
    }

    /// A winner that can't be switched to Forward=1 (unreachable here) must
    /// not replace the leader when there is no forwarder to fall back to.
    #[test]
    fn settle_keeps_leader_when_winner_cannot_forward() {
        let nt: NamespaceTrackname = "ns--track".parse().unwrap();
        let mut track = LocalTrack::new(8, 64, 16, None);
        let mut leader = TrackSubscription::new(1, false, SubscribeOptions::default());
        leader.race = true;
        leader.request_id = Some(RequestId(0));
        track.subscriptions.push(leader);
        let mut race = race_state(RaceMode::Keep, Some(ActiveRace {
            target: Location { group: 0, object: 0 },
            participants: HashMap::new(),
            forwarder: None,
            deadline: Some(Instant::now() + RACE_TIMEOUT),
        }));
        race.established.insert(2, RaceCandidate { request_id: RequestId(4), track_alias: Some(TrackAlias(7)) });
        track.race = Some(race);

        settle_probe_race(&nt, &mut track, 2, &mut ConnMap::new());

        assert!(track.subscriptions.iter().any(|s| s.client_id == 1 && s.race), "leader must stay");
        let race = track.race.as_ref().unwrap();
        assert!(race.established.contains_key(&2));
        assert!(race.active.as_ref().unwrap().deadline.unwrap() > Instant::now(), "retried later, not every tick");
    }

    /// A handover starts at the leader's first partial object, not at an
    /// older stalled one from another source.
    #[test]
    fn race_starts_at_the_leaders_first_partial_object() {
        let mut track = LocalTrack::new(8, 64, 16, None);
        let loc = |object| Location { group: 0, object };
        for object in 0..3 {
            track.cache.begin(NewObject::new(loc(object), 4)).unwrap();
        }
        let mut leader = TrackSubscription::new(1, false, SubscribeOptions::default());
        leader.race = true;
        leader.in_progress = vec![(loc(2), 4), (loc(1), 4)];
        track.subscriptions.push(leader);
        assert_eq!(race_start(&track), loc(1));

        track.subscriptions.clear();
        assert_eq!(race_start(&track), loc(3), "no leader: right after the cache");
    }

    #[test]
    fn pending_candidate_is_no_standby_yet() {
        let mut race = race_state(RaceMode::Data, None);
        race.established.insert(2, RaceCandidate { request_id: RequestId(4), track_alias: None });
        assert!(!race.has_standby());
        race.established.insert(3, RaceCandidate { request_id: RequestId(6), track_alias: Some(TrackAlias(1)) });
        assert!(race.has_standby());
    }

    /// Without any result, a `Data` race ends at its deadline.
    #[test]
    fn data_race_times_out() {
        let nt: NamespaceTrackname = "ns--track".parse().unwrap();
        let mut track = LocalTrack::new(8, 64, 16, None);
        let active = |deadline| ActiveRace {
            target: Location { group: 0, object: 0 },
            participants: HashMap::new(),
            forwarder: None,
            deadline: Some(deadline),
        };

        track.race = Some(race_state(RaceMode::Data, Some(active(Instant::now() + RACE_TIMEOUT))));
        poll_active_race(&nt, &mut track, &mut ConnMap::new());
        assert!(track.race.as_ref().unwrap().active.is_some());

        track.race = Some(race_state(RaceMode::Data, Some(active(Instant::now()))));
        poll_active_race(&nt, &mut track, &mut ConnMap::new());
        assert!(track.race.as_ref().unwrap().active.is_none());
    }
}

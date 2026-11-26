//! Which connections a concrete `--sub` track subscribes to.

use super::*;
use quiche_moq::SubscribeOptions;

/// Connections that announced `ns` or a prefix of it (draft-ietf-moq-transport-18 §8).
fn announcers<'a>(ad: &'a AppData, ns: &'a Namespace) -> impl Iterator<Item = ClientId> + 'a {
    ad.namespaces.iter().filter(|(announced, _)| ns.has_prefix(announced)).flat_map(|(_, owners)| owners.iter().copied())
}

// TODO draft-ietf-moq-transport-18 §8: relays MUST send a SUBSCRIBE to every matching publisher,
//  relaying picks one; changing it changes the testbed's relays, implement when needed
/// Single candidate for `ns`: smallest `ClientId` among announcers, else
/// smallest `#upstream`. IDs assign sequentially, so "smallest" ≈ "earliest known".
pub(super) fn pick_one(ad: &AppData, ns: &Namespace) -> Option<ClientId> {
    announcers(ad, ns).min()
        .or_else(|| ad.upstream_cids.iter().min().copied())
}

/// Adds a `TrackSubscription` per concrete `--sub` template for any new
/// candidate: every announcer + `#upstream` (`#multi`), or just one, picked
/// once and sticky (non-`#multi`) — a later smaller-`ClientId` candidate
/// doesn't replace it. Never removes a subscription. Called on new candidate
/// info (announcement, or a connection newly `#upstream`) — a track that
/// lost its sole source stays sourceless until a genuinely new one appears,
/// and then resumes at `LocalTrack::resume_location`.
pub(super) fn sync_sub_track_templates(ad: &mut AppData) {
    let (cache_depth, cache_max_obj, cache_avg_obj) = (ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj);
    for t in ad.sub_track_templates.clone() {
        // Non-multi (including #race): an existing subscription must stick,
        // whichever cid it is — dedup below only catches duplicates of the
        // same cid. #race still refreshes its probe candidate list below.
        if !t.multi && ad.local_tracks.get(&t.track).is_some_and(|l| !l.subscriptions.is_empty()) {
            if t.race { refresh_race_candidates(ad, &t); }
            continue;
        }
        let candidates: Vec<ClientId> = if t.multi {
            all_candidates(ad, t.track.namespace())
        } else {
            pick_one(ad, t.track.namespace()).into_iter().collect()
        };
        if candidates.is_empty() { continue; }
        let local = ad.local_tracks.entry(t.track.clone())
            .or_insert_with(|| LocalTrack::new(cache_depth, cache_max_obj, cache_avg_obj, None));
        // lost every source: resume at the first incomplete object
        // - complete objects after it come again, deduplicated by the cache
        let resume = local.subscriptions.is_empty().then(|| local.resume_location()).flatten();
        let options = SubscribeOptions { filter: resume.map_or(t.options.filter, |start| LocationFilter::AbsoluteStart { start }), ..t.options };
        for cid in candidates {
            // an established standby takes over without a second SUBSCRIBE (`replace_lost_leader`)
            let standby = local.race.as_ref().is_some_and(|r| r.established.contains_key(&cid));
            if !standby && !local.subscriptions.iter().any(|s| s.client_id == cid) {
                let mut sub = TrackSubscription::new(cid, t.passive, options);
                sub.race = t.race;
                local.subscriptions.push(sub);
            }
        }
        if t.race {
            let race = local.race.get_or_insert_with(Default::default);
            race.mode = t.race_mode;
            race.options = t.options;
            refresh_race_candidates(ad, &t);
        }
    }
}

/// Every announcer of `ns` and every `#upstream`, sorted and deduplicated.
pub(super) fn all_candidates(ad: &AppData, ns: &Namespace) -> Vec<ClientId> {
    let mut all: Vec<ClientId> = announcers(ad, ns)
        .chain(ad.upstream_cids.iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// Refreshes a `#race` template's probe candidate list: every currently
/// known candidate for the track's namespace (same enumeration as `#multi`),
/// except whichever one currently holds the leader `TrackSubscription`.
/// Called whenever new announcement/upstream info arrives, same trigger as
/// `sync_sub_track_templates`.
pub(super) fn refresh_race_candidates(ad: &mut AppData, t: &SubTrackTemplate) {
    let all = all_candidates(ad, t.track.namespace());
    let Some(local) = ad.local_tracks.get_mut(&t.track) else { return };
    let leader = local.subscriptions.iter().find(|s| s.race).map(|s| s.client_id);
    let Some(race) = local.race.as_mut() else { return };
    race.candidates = all.into_iter().filter(|cid| Some(*cid) != leader).collect();
}

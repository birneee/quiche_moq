//! Cache to subscribers: forwarding objects and PUBLISH_DONE.

use super::*;

/// PUBLISH_DONE to the subscribers of tracks that lost all their
/// subscriptions, and drops relayed tracks nobody wants.
pub(super) fn finalize_publish_done(r: &mut Runner) {
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for track in ad.local_tracks.values_mut() {
        // what is still fetched is forwarded first
        if track.fill.running() { continue; }
        track.aliases.retain(|a| {
            let Some(status) = a.publish_done else { return true };
            if let Some((mut moq, handles)) = conn_for(conns, a.conn_id)
                && let Some(publication) = handles.publications.remove(&a.request_id)
            {
                moq.session.publish_done(publication, status, &mut moq.t);
                info!("conn {} publish done for {}", a.conn_id, a.request_id);
            }
            false
        });
    }
    ad.local_tracks.retain(|nt, track| {
        if !track.unwanted() { return true; }
        for sub in track.subscriptions.drain(..) {
            unsubscribe_upstream(conns, sub.client_id, sub.request_id);
        }
        info!("{}", format_track_stats(nt, &track.stats));
        false
    });
}

/// Advances every subscriber alias's forwarding state by one round and
/// reports whether anything actually happened. The caller loops rounds
/// until this returns `false`, so a backlog drains as fast as real capacity
/// allows instead of trickling out per external event.
///
/// Independent subgroups have no ordering relationship in MoQT, so each
/// alias can have several open concurrently (`in_flight`) — e.g.
/// representing separate join points — rather than being limited to one
/// object in flight at a time.
///
/// Visits aliases across all tracks, highest subscriber priority first
/// (draft-19 §7.2).
/// - order matters: the first writer takes quiche's `tx_cap`
/// - one pass per priority in use (256-bit set), no allocation
pub(super) fn forward_local_tracks(
    tracks: &mut HashMap<NamespaceTrackname, LocalTrack>,
    conns: &mut ConnMap<ConnAppData>,
) -> bool {
    let mut present = [0u64; 4];
    for track in tracks.values() {
        for als in &track.aliases {
            present[(als.priority >> 6) as usize] |= 1 << (als.priority & 63);
        }
    }

    let mut progress = false;
    // ascending number = highest priority first
    for (word_idx, mut word) in present.into_iter().enumerate() {
        while word != 0 {
            let priority = (word_idx as u8) * 64 + word.trailing_zeros() as u8;
            word &= word - 1;
            for track in tracks.values_mut() {
                let LocalTrack { cache, aliases, .. } = track;
                for als in aliases.iter_mut().filter(|a| a.priority == priority) {
                    progress |= forward_local_alias(cache, als, conns);
                }
            }
        }
    }
    progress
}

/// One subscriber alias's egress: pending resets, then objects.
pub(super) fn forward_local_alias(
    cache: &PackedObjectCache,
    als: &mut LocalAliasState,
    conns: &mut ConnMap<ConnAppData>,
) -> bool {
    let Some(mut moq) = moq_handle_for(conns, als.conn_id) else {
        als.state.needs_reset.clear();
        return false;
    };
    // Forward State 0 (draft-ietf-moq-transport-19 §10.2.17): established, but this subscriber
    // doesn't want Objects yet — e.g. a `#race` subscriber's persistent,
    // not-currently-leading candidate subscription (see `trigger_race`).
    // Still drain any pending reset (e.g. this same forward=0 transition
    // queued one in `apply_subscription_update`) — that's cleanup, not a new
    // send, so it has to happen even though `forward` skips everything else.
    if !als.forward {
        cache_to_moq::drain_resets(&mut als.state, &mut moq, als.alias);
        return false;
    }
    let progress = cache_to_moq::forward(cache, &mut als.state, &mut moq, als.alias, "local");
    if als.publish_done.is_none() && als.state.range_delivered(cache) {
        als.publish_done = Some(PublishDoneStatus::SubscriptionEnded);
    }
    progress
}

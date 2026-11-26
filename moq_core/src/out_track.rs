use crate::prelude::*;
use hashbrown::HashMap;

use alloc::collections::BTreeMap;
use core::time::Duration;
use moq_wire::{DEFAULT_SUBSCRIBER_PRIORITY, SubgroupLocation, Version};
use moq_wire::range_filter::ObjectFilter;

use crate::error::{Error, Result};
use crate::time::Timestamp;
use crate::transport::object_urgency;

/// The subscriber's OBJECT_DELIVERY_TIMEOUT and SUBGROUP_DELIVERY_TIMEOUT.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeliveryTimeouts {
    pub(crate) object: Option<Duration>,
    pub(crate) subgroup: Option<Duration>,
}

impl DeliveryTimeouts {
    /// From the milliseconds of the messages.
    pub(crate) fn from_millis(object: Option<u64>, subgroup: Option<u64>) -> Self {
        Self { object: object.map(Duration::from_millis), subgroup: subgroup.map(Duration::from_millis) }
    }
}

/// Track state for an egress subscription.
pub struct OutTrack {
    /// Open outgoing streams by subgroup. A subgroup can have several: object
    /// ids increase per stream, out-of-order upstream objects need another
    /// one (draft-19 §2.2).
    pub(crate) open_streams: HashMap<SubgroupLocation, Vec<u64>>,
    /// Highest object_id `send_obj_hdr` has actually sent so far, keyed
    /// by `group_id` — kept even after every subgroup for that group has
    /// closed, so a later `prior_object_id_gap` declaration for the same
    /// group can still be checked against it (draft-19 §12.9: a gap must
    /// not retroactively cover an object_id already sent).
    /// - only the newest `SENT_UP_TO_GROUPS` groups, see `may_declare_gap`
    sent_up_to: BTreeMap<u64, u64>,
    /// Subgroup streams opened so far, the stream count of PUBLISH_DONE.
    pub(crate) streams_opened: u64,
    /// PUBLISH_DONE was sent: no new objects, removed once its streams are done.
    pub(crate) ended: bool,
    /// QUIC stream urgency for this track's subgroup streams, see `object_urgency`.
    pub(crate) urgency: u8,
    /// Whether subgroup streams are scheduled incrementally (round-robin) with other
    /// streams at the same urgency level.
    pub(crate) incremental: bool,
    /// OBJECT_DELIVERY_TIMEOUT from the SUBSCRIBE, checked against object receipt.
    // TODO the publisher's own timeouts, as Track Property and on a subgroup's first object (draft-ietf-moq-transport-19 §8)
    pub(crate) delivery_timeout: Option<Duration>,
    /// SUBGROUP_DELIVERY_TIMEOUT from the SUBSCRIBE, draft 18 on: how long a FIN'd subgroup may take to be acked
    pub(crate) subgroup_delivery_timeout: Option<Duration>,
    /// Draft 18+ times a FIN'd subgroup by `subgroup_delivery_timeout`, before the delivery timeout keeps covering it
    subgroup_timeouts: bool,
    /// MAX_CACHE_DURATION of the SUBSCRIBE_OK this track was accepted with.
    pub(crate) max_cache_duration: Option<Duration>,
    /// The subscriber's Range Filters, objects not passing aren't sent
    pub(crate) object_filter: ObjectFilter,
}

impl OutTrack {
    /// Shortest of the timeouts that expire this track's streams, if any.
    pub(crate) fn expiry_timeout(&self) -> Option<Duration> {
        [self.delivery_timeout, self.max_cache_duration].into_iter().flatten().min()
    }

    /// How long a FIN'd subgroup may take to be acked.
    pub(crate) fn fin_timeout(&self) -> Option<Duration> {
        if self.subgroup_timeouts { self.subgroup_delivery_timeout } else { self.expiry_timeout() }
    }

    /// Applies the timeouts a REQUEST_UPDATE or PUBLISH_OK names, the others stay.
    pub(crate) fn update_timeouts(&mut self, timeouts: DeliveryTimeouts) {
        self.delivery_timeout = timeouts.object.or(self.delivery_timeout);
        self.subgroup_delivery_timeout = timeouts.subgroup.or(self.subgroup_delivery_timeout);
    }

    /// When an object received at `received_at` expires.
    pub(crate) fn expiry_deadline(&self, received_at: Timestamp) -> Option<Timestamp> {
        Some(received_at + self.expiry_timeout()?)
    }

    /// `Error::ObjectExpired` if an object received at `received_at` already expired at `now`.
    pub(crate) fn check_sendable(&self, received_at: Option<Timestamp>, now: Timestamp) -> Result<()> {
        match received_at.and_then(|at| self.expiry_deadline(at)) {
            Some(deadline) if now > deadline => Err(Error::ObjectExpired),
            _ => Ok(()),
        }
    }

    pub fn new(timeouts: DeliveryTimeouts, max_cache_duration: Option<Duration>, version: Version) -> Self {
        Self {
            open_streams: HashMap::default(),
            sent_up_to: BTreeMap::new(),
            streams_opened: 0,
            ended: false,
            urgency: object_urgency(DEFAULT_SUBSCRIBER_PRIORITY),
            incremental: false,
            delivery_timeout: timeouts.object,
            subgroup_delivery_timeout: timeouts.subgroup,
            subgroup_timeouts: version.draft() >= 18,
            max_cache_duration,
            object_filter: ObjectFilter::default(),
        }
    }

    pub fn writable(&self) -> bool {
        true
    }

    /// Whether a Prior Object ID Gap ending before `object_id` can be sent
    /// in `group_id` without covering an object_id already sent.
    /// - a group older than the remembered ones can't be checked: refused
    pub(crate) fn may_declare_gap(&self, group_id: u64, gap_start: u64) -> bool {
        let forgotten = self.sent_up_to.len() == SENT_UP_TO_GROUPS
            && self.sent_up_to.first_key_value().is_some_and(|(&oldest, _)| group_id < oldest);
        !forgotten && self.sent_up_to.get(&group_id).is_none_or(|&sent_up_to| gap_start > sent_up_to)
    }

    pub(crate) fn note_sent(&mut self, group_id: u64, object_id: u64) {
        let entry = self.sent_up_to.entry(group_id).or_insert(0);
        *entry = (*entry).max(object_id);
        if self.sent_up_to.len() > SENT_UP_TO_GROUPS {
            self.sent_up_to.pop_first();
        }
    }
}

/// Groups `OutTrack::sent_up_to` remembers.
const SENT_UP_TO_GROUPS: usize = 64;

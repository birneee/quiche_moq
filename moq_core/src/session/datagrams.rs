//! Objects sent whole in datagrams (draft-ietf-moq-transport-18 §11.3).

use super::*;
use moq_wire::object::ObjectStatus;
use moq_wire::range_filter::FilteredObject;

/// An Object for `send_datagram_obj`.
#[derive(Clone, Copy, Debug)]
pub struct OutgoingDatagram<'a> {
    pub location: Location,
    pub publisher_priority: u8,
    /// No Object after this one in its Group.
    pub end_of_group: bool,
    /// The Object Status, an Object other than Normal has no payload.
    pub status: ObjectStatus,
    pub properties: &'a KeyValuePairs,
    /// When it was received, it isn't sent once expired, see `OutgoingObject::received_at`.
    pub received_at: Option<Timestamp>,
    pub payload: &'a [u8],
}

impl<Q: DropQueue> MoqTransportSession<Q> {
    /// Sends `object` in one datagram on the subscription of `track_alias`.
    /// - [`Error::Done`]: it doesn't fit into a datagram or there is no room, it is dropped (§11.3)
    /// - [`Error::Unimplemented`]: the draft or the transport has no datagrams
    pub fn send_datagram_obj(&mut self, track_alias: TrackAlias, object: OutgoingDatagram<'_>, t: &mut impl Transport) -> Result<()> {
        let OutgoingDatagram { location, publisher_priority, end_of_group, status, properties, received_at, payload } = object;
        let version = self.effective_version();
        if !ObjectDatagram::accepts(version) {
            return Err(Error::Unimplemented);
        }
        let track = self.out_tracks.get(&track_alias).filter(|track| !track.ended).ok_or(Error::TrackEnded)?;
        track.check_sendable(received_at, self.now)?;
        if !track.object_filter.passes(&FilteredObject { subgroup_id: None, object_id: location.object, publisher_priority, properties }) {
            return Err(Error::ObjectFiltered);
        }
        let datagram = ObjectDatagram {
            track_alias,
            location,
            publisher_priority: Some(publisher_priority),
            end_of_group,
            properties: properties.clone(),
            status: (status != ObjectStatus::Normal).then_some(status),
        };
        match t.send_datagram(&datagram.encode(payload, version)?) {
            Ok(()) => Ok(()),
            Err(StreamError::Failed) => Err(Error::Unimplemented),
            Err(e) => Err(e.into()),
        }
    }

    /// Queues the Objects received in datagrams on their tracks, a malformed one closes the session.
    /// - an unknown track's wait for its SUBSCRIBE_OK or PUBLISH, as streams do (§11.3)
    pub(super) fn recv_datagrams(&mut self, t: &mut impl Transport) -> ControlFlow<()> {
        let version = self.effective_version();
        while let Some(mut datagram) = t.recv_datagram() {
            match ObjectDatagram::decode(&datagram, version) {
                Ok((object, payload)) => {
                    let header_len = datagram.len() - payload.len();
                    datagram.drain(..header_len);
                    let payload = datagram;
                    match self.in_tracks.get_mut(&object.track_alias) {
                        Some(track) => track.push_datagram(object, payload),
                        None => self.park_datagram(object, payload),
                    }
                }
                Err(moq_wire::Error::Unsupported(_)) => debug!("dropping a datagram, none in draft {}", version.draft()),
                Err(e) => return self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed datagram: {e:?}"), t),
            }
        }
        ControlFlow::Continue(())
    }

    /// Keeps the Object of a datagram for a track not known yet, unless it was dropped or too
    /// many wait already.
    fn park_datagram(&mut self, object: ObjectDatagram, payload: Vec<u8>) {
        let alias = object.track_alias;
        let known = self.pending_datagrams.contains_key(&alias);
        if self.dropped_aliases.contains_key(&alias) || !known && self.pending_datagrams.len() == MAX_PENDING_DATAGRAM_TRACKS {
            debug!("dropping a datagram of track alias {alias:?}");
            return;
        }
        let now = self.now;
        let (_, pending) = self.pending_datagrams.entry(alias).or_insert_with(|| (now, VecDeque::new()));
        if pending.len() == MAX_PENDING_DATAGRAMS {
            pending.pop_front();
        }
        pending.push_back((object, payload));
    }
}

/// The datagrams kept per track not known yet, the oldest give way.
const MAX_PENDING_DATAGRAMS: usize = 64;
/// The unknown tracks datagrams are kept for, against a peer filling memory with aliases.
const MAX_PENDING_DATAGRAM_TRACKS: usize = 16;

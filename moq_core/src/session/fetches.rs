//! FETCH (draft-ietf-moq-transport-19 §10.12): ours, read from their fetch stream, and the peer's, answered on one.

use super::*;
use moq_wire::control_message::{FetchCancelMessage, FetchErrorMessage, FetchKind, FetchMessage, FetchOkMessage};
use moq_wire::fetch::{FetchEntry, FetchStream};

/// How to fetch, by default as the spec's defaults in ascending group order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchOptions {
    pub priority: u8,
    /// always named, the fetch stream's Group ID deltas depend on it
    pub group_order: GroupOrder,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self { priority: DEFAULT_SUBSCRIBER_PRIORITY, group_order: GroupOrder::Ascending }
    }
}

/// Our FETCH.
pub(super) struct InFetch {
    group_order: GroupOrder,
    /// its fetch stream, once it arrived
    stream_id: Option<u64>,
    /// the fetch stream ended, every object was read
    finished: bool,
}

/// The stream answering a peer's FETCH: opened with the first write, FETCH_HEADER first.
pub(super) struct OutFetch {
    request_id: RequestId,
    version: Version,
    stream_id: Option<u64>,
    codec: FetchStream,
    /// written, not yet taken by the transport
    pending: Vec<u8>,
    remaining_payload: usize,
}

impl OutFetch {
    fn new(request_id: RequestId, version: Version, group_order: GroupOrder) -> Self {
        Self { request_id, version, stream_id: None, codec: FetchStream::new(version, group_order), pending: Vec::new(), remaining_payload: 0 }
    }

    /// Opens the stream with its FETCH_HEADER and hands the transport what is pending, `Error::Done` while some is left.
    fn flush(&mut self, t: &mut impl Transport) -> Result<u64> {
        let stream_id = match self.stream_id {
            Some(stream_id) => stream_id,
            None => {
                let stream_id = match t.open_uni_stream() {
                    Ok(stream_id) => stream_id,
                    // stream limit reached, retry later
                    Err(StreamError::Done) => return Err(Error::Done),
                    Err(e) => return Err(e.into()),
                };
                let mut header = Vec::new();
                FetchStream::encode_header(&mut self.version.writer(&mut header), self.request_id)?;
                header.append(&mut self.pending);
                self.pending = header;
                self.stream_id = Some(stream_id);
                stream_id
            }
        };
        while !self.pending.is_empty() {
            match t.send_stream(stream_id, &self.pending, false) {
                Ok(n) => { self.pending.drain(..n); }
                Err(StreamError::Done) => return Err(Error::Done),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(stream_id)
    }

    fn send_entry(&mut self, entry: &FetchEntry, t: &mut impl Transport) -> Result<()> {
        if self.remaining_payload > 0 {
            return Err(Error::UnfinishedPayload);
        }
        self.flush(t)?;
        self.codec.encode(&mut self.version.writer(&mut self.pending), entry)?;
        if let FetchEntry::Object(object) = entry {
            self.remaining_payload = object.payload_len;
        }
        // taken by the next flush if the transport has no room
        let _ = self.flush(t);
        Ok(())
    }

    fn send_payload(&mut self, buf: &[u8], t: &mut impl Transport) -> Result<usize> {
        let stream_id = self.flush(t)?;
        let len = buf.len().min(self.remaining_payload);
        if len == 0 {
            return Err(Error::ExceededPayload);
        }
        let n = match t.send_stream(stream_id, &buf[..len], false) {
            Ok(n) => n,
            Err(StreamError::Done) => return Err(Error::Done),
            Err(e) => return Err(e.into()),
        };
        self.remaining_payload -= n;
        Ok(n)
    }

    /// Sends what is pending and the FIN, `false` until the transport took all.
    fn finish(&mut self, t: &mut impl Transport) -> bool {
        match self.flush(t) {
            Ok(stream_id) => !matches!(t.send_stream(stream_id, &[], true), Err(StreamError::Done)),
            Err(Error::Done) => false,
            Err(_) => true,
        }
    }

    fn reset(&self, t: &mut impl Transport) {
        if let Some(stream_id) = self.stream_id {
            t.reset_stream(stream_id, ResetCode::Canceled);
        }
    }
}

impl<Q: DropQueue> MoqTransportSession<Q> {
    /// Fetches published objects, answered by `Event::FetchOk` or `Event::RequestFailed`, the objects read
    /// with `recv_fetch_entry`.
    // TODO before draft 15, whose fetch stream names every field of each object
    pub fn fetch(&mut self, kind: FetchKind, options: FetchOptions, t: &mut impl Transport) -> Result<Fetching<Q>> {
        if !self.effective_version().has_generic_replies() {
            return Err(Error::Unimplemented);
        }
        let request_id = self.available_request_id()?;
        let cm = FetchMessage { subscriber_priority: options.priority, group_order: Some(options.group_order), ..FetchMessage::new(request_id, kind) };
        self.send_request(request_id, &ControlMessageEnum::Fetch(cm), t)?;
        self.fetches.insert(request_id, InFetch { group_order: options.group_order, stream_id: None, finished: false });
        self.next_request_id = self.next_request_id.next();
        Ok(Fetching { obligation: self.obligation(DroppedKind::Fetching, request_id) })
    }

    /// The next entry of `fetch`'s stream, an object's payload read with `recv_fetch_pld`.
    /// - `Error::Done` until more arrived, `Error::Fin` once every object was read
    pub fn recv_fetch_entry(&mut self, fetch: &Fetching<Q>, t: &mut impl Transport) -> Result<FetchEntry> {
        self.assert_created(&fetch.obligation);
        let stream_id = self.fetch_stream(fetch.request_id())?;
        let read = self.in_streams.get_mut(&stream_id).ok_or(Error::Fin)?.read_fetch_entry(t);
        self.fetch_read(fetch.request_id(), stream_id, read, t)
    }

    /// More payload of the object `recv_fetch_entry` returned last, `Error::Done` until more arrived.
    pub fn recv_fetch_pld(&mut self, fetch: &Fetching<Q>, buf: &mut [u8], t: &mut impl Transport) -> Result<usize> {
        self.assert_created(&fetch.obligation);
        let stream_id = self.fetch_stream(fetch.request_id())?;
        let read = self.in_streams.get_mut(&stream_id).ok_or(Error::Fin)?.read_obj_pld(t, buf);
        self.fetch_read(fetch.request_id(), stream_id, read, t)
    }

    fn fetch_stream(&self, request_id: RequestId) -> Result<u64> {
        let fetch = self.fetches.get(&request_id).ok_or(Error::Fin)?;
        if fetch.finished {
            return Err(Error::Fin);
        }
        fetch.stream_id.ok_or(Error::Done)
    }

    /// The outcome of reading fetch stream `stream_id`: its end finishes the fetch, a malformed one closes the session.
    fn fetch_read<T>(&mut self, request_id: RequestId, stream_id: u64, read: Result<T>, t: &mut impl Transport) -> Result<T> {
        match read {
            Err(Error::Fin) => {
                self.in_streams.remove(&stream_id);
                if let Some(fetch) = self.fetches.get_mut(&request_id) {
                    fetch.finished = true;
                }
                Err(Error::Fin)
            }
            Err(Error::Wire(e)) => {
                let _ = self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed fetch stream: {e:?}"), t);
                Err(Error::Wire(e))
            }
            read => read,
        }
    }

    /// Ends `fetch`, cancelling it unless every object was read.
    pub fn cancel_fetch(&mut self, mut fetch: Fetching<Q>, t: &mut impl Transport) {
        let request_id = self.resolve(&mut fetch.obligation);
        self.end_fetching(request_id, t);
    }

    pub(super) fn end_fetching(&mut self, request_id: RequestId, t: &mut impl Transport) {
        let Some(fetch) = self.fetches.remove(&request_id) else { return };
        if fetch.finished {
            self.control_channel.end_request_stream(request_id, t);
            return;
        }
        if let Some(stream_id) = fetch.stream_id
            && self.in_streams.remove(&stream_id).is_some()
        {
            self.stop_in_stream(stream_id, t);
        }
        // FETCH_CANCEL until draft 16, cancelling the request after
        if self.control_channel.uses_per_request_streams() {
            self.cancel_request_stream(request_id, t);
        } else {
            self.send_control_message(t, &ControlMessageEnum::FetchCancel(FetchCancelMessage { request_id }));
        }
    }

    /// The fetch stream answering our FETCH `request_id` arrived, one answering none is stopped.
    pub(super) fn attach_fetch_stream(&mut self, stream_id: u64, request_id: RequestId, t: &mut impl Transport) {
        let version = self.effective_version();
        match self.fetches.get_mut(&request_id) {
            Some(fetch) if fetch.stream_id.is_none_or(|id| id == stream_id) => {
                if fetch.stream_id.is_none() {
                    fetch.stream_id = Some(stream_id);
                    if let Some(stream) = self.in_streams.get_mut(&stream_id) {
                        stream.start_fetch(FetchStream::new(version, fetch.group_order));
                    }
                }
            }
            _ => {
                self.in_streams.remove(&stream_id);
                self.stop_in_stream(stream_id, t);
            }
        }
    }

    /// Our FETCH failed: forgets it and stops its stream, `false` if it is none.
    pub(super) fn fail_fetch(&mut self, request_id: RequestId, t: &mut impl Transport) -> bool {
        let Some(fetch) = self.fetches.remove(&request_id) else { return false };
        if let Some(stream_id) = fetch.stream_id
            && self.in_streams.remove(&stream_id).is_some()
        {
            self.stop_in_stream(stream_id, t);
        }
        true
    }

    /// Why a peer's FETCH is refused before it is handed out.
    pub(super) fn fetch_refusal(&self, cm: &FetchMessage) -> Option<RequestErrorCode> {
        if !self.effective_version().has_generic_replies() {
            // TODO fetch streams before draft 15
            return Some(RequestErrorCode::NotSupported);
        }
        // §10.12.2: joining an Established subscription or a Pending one, the caller holds it until answered
        let joinable = |id: &RequestId| self.out_requests.contains_key(id) || matches!(self.unanswered_requests.get(id), Some(Unanswered::Subscribe { .. }));
        if let FetchKind::RelativeJoining { joining_request_id, .. } | FetchKind::AbsoluteJoining { joining_request_id, .. } = cm.kind
            && !joinable(&joining_request_id)
        {
            return Some(RequestErrorCode::InvalidJoiningRequestId);
        }
        self.object_filter(&cm.range_filters).is_none().then_some(RequestErrorCode::InvalidFilter)
    }

    /// FETCH_ERROR until draft 14.
    pub(super) fn fetch_error(&self, request_id: RequestId, error_code: RequestErrorCode) -> Option<ControlMessageEnum> {
        (!self.effective_version().has_generic_replies()).then(|| ControlMessageEnum::FetchError(FetchErrorMessage::new(request_id, error_code)))
    }

    pub(crate) fn accept_fetch(&mut self, request_id: RequestId, cm: FetchMessage, ok: crate::request::FetchOk, t: &mut impl Transport) -> FetchResponse<Q> {
        let version = self.effective_version();
        // TODO the publisher's DEFAULT_PUBLISHER_GROUP_ORDER
        let group_order = cm.group_order.unwrap_or(GroupOrder::Ascending);
        let reply = FetchOkMessage::new(request_id, ok.end_of_track, ok.end_location, group_order);
        self.send_response(request_id, &ControlMessageEnum::FetchOk(reply), t);
        self.out_fetches.insert(request_id, OutFetch::new(request_id, version, group_order));
        FetchResponse { obligation: self.obligation(DroppedKind::FetchResponse, request_id) }
    }

    /// Writes the next entry of `response`'s stream, an object's payload follows with `send_fetch_pld`.
    /// - in the requested group order, within a group by Object ID
    /// - `Error::TrackEnded` once the subscriber cancelled the fetch
    pub fn send_fetch_entry(&mut self, response: &FetchResponse<Q>, entry: &FetchEntry, t: &mut impl Transport) -> Result<()> {
        self.assert_created(&response.obligation);
        self.out_fetches.get_mut(&response.request_id()).ok_or(Error::TrackEnded)?.send_entry(entry, t)
    }

    /// Writes payload of the object `send_fetch_entry` wrote last, as much as the transport takes.
    pub fn send_fetch_pld(&mut self, response: &FetchResponse<Q>, buf: &[u8], t: &mut impl Transport) -> Result<usize> {
        self.assert_created(&response.obligation);
        self.out_fetches.get_mut(&response.request_id()).ok_or(Error::TrackEnded)?.send_payload(buf, t)
    }

    /// Ends `response`'s stream with a FIN once the transport took what is written.
    /// - an object's payload unfinished: the stream is reset, `Error::UnfinishedPayload`
    pub fn finish_fetch(&mut self, mut response: FetchResponse<Q>, t: &mut impl Transport) -> Result<()> {
        let request_id = self.resolve(&mut response.obligation);
        let out = self.out_fetches.remove(&request_id).ok_or(Error::TrackEnded)?;
        self.control_channel.end_request_stream(request_id, t);
        if out.remaining_payload > 0 {
            out.reset(t);
            return Err(Error::UnfinishedPayload);
        }
        self.finishing_fetches.push(out);
        self.flush_finishing_fetches(t);
        Ok(())
    }

    /// Sends what finished fetch streams still have pending, then their FIN.
    pub(super) fn flush_finishing_fetches(&mut self, t: &mut impl Transport) {
        self.finishing_fetches.retain_mut(|out| !out.finish(t));
    }

    /// Our answer to `request_id` was dropped or cancelled by the subscriber: its stream is reset.
    pub(super) fn reset_fetch_response(&mut self, request_id: RequestId, t: &mut impl Transport) -> bool {
        let Some(out) = self.out_fetches.remove(&request_id) else { return false };
        out.reset(t);
        true
    }
}

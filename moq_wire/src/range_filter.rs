//! Range Filters (draft-ietf-moq-transport-19 §5.1.3), parameters of SUBSCRIBE, PUBLISH_OK and REQUEST_UPDATE.

use crate::prelude::*;
use crate::message_parameters::{MessageParams, Param};
use crate::{Frame, KeyValuePairs, Reader, VarintKey, Version};

/// The range filter parameters as received, see [`RangeFilters::parse`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeFilters(Vec<(Param, Vec<u8>)>);

impl RangeFilters {
    const PARAMS: [Param; 5] = [Param::SubgroupFilter, Param::ObjectIdFilter, Param::PriorityFilter, Param::ObjectPropertyFilter, Param::TrackPropertyFilter];

    /// `filters` as parameters.
    pub fn new(filters: &[RangeFilter], version: Version) -> crate::Result<Self> {
        filters.iter().map(|filter| filter.encode(version)).collect::<crate::Result<_>>().map(Self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn take(params: &mut MessageParams) -> Self {
        Self(Self::PARAMS.into_iter().flat_map(|param| params.take_all_bytes(param).into_iter().map(move |bytes| (param, bytes))).collect())
    }

    pub(crate) fn put(&self, params: &mut MessageParams) {
        for (param, bytes) in &self.0 {
            params.put_bytes(*param, bytes.clone());
        }
    }

    /// The filter these parameters describe, `None` if one is invalid: the peer gets INVALID_FILTER then.
    pub fn parse(&self, version: Version) -> Option<ObjectFilter> {
        let mut filters: Vec<RangeFilter> = Vec::new();
        for (param, bytes) in &self.0 {
            let filter = RangeFilter::decode(*param, bytes, version)?;
            // the same Parameter Type, SetID and Property Type only once
            if filters.iter().any(|f| f.set_id == filter.set_id && f.kind == filter.kind) {
                return None;
            }
            filters.push(filter);
        }
        Some(ObjectFilter(filters))
    }
}

/// What a range filter selects by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFilterKind {
    Subgroup,
    ObjectId,
    Priority,
    ObjectProperty(VarintKey),
    /// selects tracks of SUBSCRIBE_TRACKS, not objects
    TrackProperty(VarintKey),
}

/// One range filter parameter: a value within one of its inclusive ranges passes, the last can be open-ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeFilter {
    pub set_id: u8,
    pub kind: RangeFilterKind,
    pub ranges: Vec<(u64, Option<u64>)>,
}

impl RangeFilter {
    /// `{ SetID, [Property Type], Range... }` with Start delta encoded from the prior End, End from its Start.
    fn decode(param: Param, bytes: &[u8], version: Version) -> Option<Self> {
        let mut r = version.frame_reader(bytes);
        let set_id = r.u8().ok()?;
        let property = |r: &mut Reader<'_, Frame>| {
            let ty = r.varint().ok()?;
            // a single integer value
            (ty % 2 == 0).then(|| VarintKey::new(ty))
        };
        let kind = match param {
            Param::SubgroupFilter => RangeFilterKind::Subgroup,
            Param::ObjectIdFilter => RangeFilterKind::ObjectId,
            Param::PriorityFilter => RangeFilterKind::Priority,
            Param::ObjectPropertyFilter => RangeFilterKind::ObjectProperty(property(&mut r)?),
            Param::TrackPropertyFilter => RangeFilterKind::TrackProperty(property(&mut r)?),
            _ => return None,
        };
        let mut ranges = Vec::new();
        let mut previous_end = 0u64;
        while r.remaining() > 0 {
            let start = previous_end.checked_add(r.varint().ok()?)?;
            if r.remaining() == 0 {
                ranges.push((start, None));
                break;
            }
            let end = start.checked_add(r.varint().ok()?)?;
            ranges.push((start, Some(end)));
            previous_end = end;
        }
        // Publisher Priority is 8 bits
        let beyond_priority = kind == RangeFilterKind::Priority && ranges.iter().any(|&(start, end)| start > 255 || end.is_some_and(|end| end > 255));
        (!ranges.is_empty() && !beyond_priority).then_some(Self { set_id, kind, ranges })
    }

    fn encode(&self, version: Version) -> crate::Result<(Param, Vec<u8>)> {
        let mut bytes = Vec::new();
        let mut w = version.writer(&mut bytes);
        w.u8(self.set_id);
        let param = match self.kind {
            RangeFilterKind::Subgroup => Param::SubgroupFilter,
            RangeFilterKind::ObjectId => Param::ObjectIdFilter,
            RangeFilterKind::Priority => Param::PriorityFilter,
            RangeFilterKind::ObjectProperty(key) => {
                w.varint(key.get())?;
                Param::ObjectPropertyFilter
            }
            RangeFilterKind::TrackProperty(key) => {
                w.varint(key.get())?;
                Param::TrackPropertyFilter
            }
        };
        let mut previous_end = 0;
        for &(start, end) in &self.ranges {
            let invalid = || crate::Error::Unencodable("Range Filter ranges out of order");
            w.varint(start.checked_sub(previous_end).ok_or_else(invalid)?)?;
            let Some(end) = end else { break };
            w.varint(end.checked_sub(start).ok_or_else(invalid)?)?;
            previous_end = end;
        }
        Ok((param, bytes))
    }

    fn contains(&self, value: u64) -> bool {
        self.ranges.iter().any(|&(start, end)| start <= value && end.is_none_or(|end| value <= end))
    }

    fn passes(&self, object: &FilteredObject<'_>) -> bool {
        match self.kind {
            RangeFilterKind::Subgroup => object.subgroup_id.is_some_and(|id| self.contains(id)),
            RangeFilterKind::ObjectId => self.contains(object.object_id),
            RangeFilterKind::Priority => self.contains(object.publisher_priority.into()),
            RangeFilterKind::ObjectProperty(key) => object.properties.get_varint(key).is_some_and(|value| self.contains(value)),
            RangeFilterKind::TrackProperty(_) => true,
        }
    }
}

/// What range filters look at of an object.
pub struct FilteredObject<'a> {
    /// `None` for a datagram, it belongs to no Subgroup (draft-ietf-moq-transport-19 §2)
    pub subgroup_id: Option<u64>,
    pub object_id: u64,
    pub publisher_priority: u8,
    pub properties: &'a KeyValuePairs,
}

/// The range filters of a subscription, passing every object without any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectFilter(Vec<RangeFilter>);

impl ObjectFilter {
    /// Ranges in all filters, limited by MAX_FILTER_RANGES.
    pub fn range_count(&self) -> usize {
        self.0.iter().map(|filter| filter.ranges.len()).sum()
    }

    /// The filters of a SetID combine with AND, the SetIDs with OR.
    pub fn passes(&self, object: &FilteredObject<'_>) -> bool {
        let set_passes = |set_id| self.0.iter().filter(|filter| filter.set_id == set_id).all(|filter| filter.passes(object));
        self.0.is_empty() || self.0.iter().any(|filter| set_passes(filter.set_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyValuePair, MOQ_VERSION_DRAFT_19};

    fn filters(params: &[(Param, &[u8])]) -> RangeFilters {
        RangeFilters(params.iter().map(|&(param, bytes)| (param, bytes.to_vec())).collect())
    }

    fn object(subgroup_id: u64, object_id: u64, properties: &KeyValuePairs) -> FilteredObject<'_> {
        FilteredObject { subgroup_id: Some(subgroup_id), object_id, publisher_priority: 0, properties }
    }

    /// §5.1.3: ranges 3-5 and 10-15 are Start 3, End 2, Start 5, End 5.
    #[test]
    fn ranges_are_delta_encoded() {
        let filter = filters(&[(Param::ObjectIdFilter, &[0, 3, 2, 5, 5])]).parse(MOQ_VERSION_DRAFT_19).unwrap();
        let none = KeyValuePairs::new();
        let passing: Vec<u64> = (0..20).filter(|&id| filter.passes(&object(0, id, &none))).collect();
        assert_eq!(passing, [3, 4, 5, 10, 11, 12, 13, 14, 15]);
        assert_eq!(filter.range_count(), 2);
    }

    #[test]
    fn last_end_can_be_omitted() {
        let filter = filters(&[(Param::SubgroupFilter, &[0, 7])]).parse(MOQ_VERSION_DRAFT_19).unwrap();
        let none = KeyValuePairs::new();
        assert!(!filter.passes(&object(6, 0, &none)));
        assert!(filter.passes(&object(1000, 0, &none)));
    }

    /// The filters of a SetID combine with AND, SetIDs with OR.
    #[test]
    fn sets_combine() {
        // set 0: subgroup 1 and object 0-1, set 1: subgroup 5
        let filter = filters(&[(Param::SubgroupFilter, &[0, 1, 0]), (Param::ObjectIdFilter, &[0, 0, 1]), (Param::SubgroupFilter, &[1, 5, 0])])
            .parse(MOQ_VERSION_DRAFT_19)
            .unwrap();
        let none = KeyValuePairs::new();
        assert!(filter.passes(&object(1, 1, &none)));
        assert!(!filter.passes(&object(1, 2, &none)));
        assert!(filter.passes(&object(5, 9, &none)));
        assert!(!filter.passes(&object(2, 0, &none)));
    }

    #[test]
    fn object_property_filter_needs_the_property() {
        let filter = filters(&[(Param::ObjectPropertyFilter, &[0, 0x0A, 4, 0])]).parse(MOQ_VERSION_DRAFT_19).unwrap();
        let with = [KeyValuePair::Varint(VarintKey::new(0x0A), 4)].into_iter().collect();
        assert!(filter.passes(&object(0, 0, &with)));
        assert!(!filter.passes(&object(0, 0, &KeyValuePairs::new())));
    }

    #[test]
    fn round_trips() {
        let filter = RangeFilter { set_id: 2, kind: RangeFilterKind::ObjectProperty(VarintKey::new(0x0A)), ranges: vec![(3, Some(5)), (10, None)] };
        let parsed = RangeFilters::new(core::slice::from_ref(&filter), MOQ_VERSION_DRAFT_19).unwrap().parse(MOQ_VERSION_DRAFT_19).unwrap();
        assert_eq!(parsed, ObjectFilter(vec![filter]));
    }

    #[test]
    fn invalid_filters_are_refused() {
        for invalid in [
            filters(&[(Param::ObjectPropertyFilter, &[0, 0x0B, 4])]),
            filters(&[(Param::PriorityFilter, &[0, 0x81, 0x2C])]),
            filters(&[(Param::SubgroupFilter, &[0, 1]), (Param::SubgroupFilter, &[0, 2])]),
            filters(&[(Param::SubgroupFilter, &[0])]),
        ] {
            assert_eq!(invalid.parse(MOQ_VERSION_DRAFT_19), None, "{invalid:?}");
        }
    }
}

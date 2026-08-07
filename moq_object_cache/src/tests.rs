use rstest::rstest;
use moq_wire::{KeyValuePairs, Location};
#[cfg(test)]
use crate::ObjectCache;
use crate::{PackedObjectCache, SlotObjectCache};

fn loc(group: u64, object: u64) -> Location { Location { group, object } }
fn kv() -> KeyValuePairs { KeyValuePairs::new() }

fn packed() -> Box<dyn ObjectCache> { Box::new(PackedObjectCache::new(4, 64, 64)) }
fn slot()   -> Box<dyn ObjectCache> { Box::new(SlotObjectCache::new(4, 64)) }

#[rstest]
fn empty(#[values(packed(), slot())] cache: Box<dyn ObjectCache>) {
    assert!(cache.is_empty());
    assert!(cache.latest().is_none());
    assert!(cache.find(loc(0, 0)).is_none());
    assert!(cache.next_after(None).is_none());
}

#[rstest]
fn partial_object(#[values(packed(), slot())] mut cache: Box<dyn ObjectCache>) {
    cache.begin(loc(0, 0), kv(), 10).unwrap();
    assert!(!cache.latest().unwrap().is_complete());
    assert_eq!(cache.latest().unwrap().readable_from(0), &[] as &[u8]);
    {
        let mut o = cache.latest_mut().unwrap();
        o.unfilled()[..3].copy_from_slice(&[1, 2, 3]);
        o.advance_write_pos(3);
    }
    assert_eq!(cache.latest().unwrap().readable_from(0), &[1, 2, 3]);
    assert_eq!(cache.latest().unwrap().readable_from(2), &[3]);
    {
        let mut o = cache.latest_mut().unwrap();
        o.unfilled()[..7].copy_from_slice(&[4, 5, 6, 7, 8, 9, 10]);
        o.advance_write_pos(7);
    }
    assert!(cache.latest().unwrap().is_complete());
    assert_eq!(cache.latest().unwrap().readable_from(0), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
}

#[rstest]
fn eviction_and_order(
    #[values(
        Box::new(PackedObjectCache::new(3, 8, 8)) as Box<dyn ObjectCache>,
        Box::new(SlotObjectCache::new(3, 8)) as Box<dyn ObjectCache>,
    )]
    mut cache: Box<dyn ObjectCache>,
) {
    for i in 0..4u64 {
        cache.begin(loc(0, i), kv(), 1).unwrap();
        let mut o = cache.latest_mut().unwrap();
        o.unfilled()[0] = i as u8;
        o.advance_write_pos(1);
    }
    assert!(cache.find(loc(0, 0)).is_none());
    assert!(cache.find(loc(0, 3)).is_some());
}

#[rstest]
fn next_after_and_find(
    #[values(
        Box::new(PackedObjectCache::new(4, 8, 8)) as Box<dyn ObjectCache>,
        Box::new(SlotObjectCache::new(4, 8)) as Box<dyn ObjectCache>,
    )]
    mut cache: Box<dyn ObjectCache>,
) {
    for i in 0..3u64 {
        cache.begin(loc(0, i), kv(), 1).unwrap();
        let mut o = cache.latest_mut().unwrap();
        o.unfilled()[0] = i as u8;
        o.advance_write_pos(1);
    }
    assert_eq!(cache.next_after(None).unwrap().location(), loc(0, 0));
    assert_eq!(cache.next_after(Some(loc(0, 0))).unwrap().location(), loc(0, 1));
    assert!(cache.next_after(Some(loc(0, 2))).is_none());
    assert_eq!(cache.find(loc(0, 1)).unwrap().readable_from(0), &[1]);
}

#[rstest]
fn unfilled_direct_write(#[values(packed(), slot())] mut cache: Box<dyn ObjectCache>) {
    cache.begin(loc(0, 0), kv(), 4).unwrap();
    {
        let mut obj = cache.latest_mut().unwrap();
        obj.unfilled().copy_from_slice(&[10, 20, 30, 40]);
        obj.advance_write_pos(4);
    }
    assert!(cache.latest().unwrap().is_complete());
    assert_eq!(cache.latest().unwrap().readable_from(0), &[10, 20, 30, 40]);
}

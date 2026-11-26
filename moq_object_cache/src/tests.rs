use rstest::rstest;
use moq_wire::Location;
#[cfg(test)]
use crate::{ObjectCache, ObjectState};
use crate::{begin_or_join, fold, JoinError, Level, LevelSlotObjectCache, NewObject, PackedObjectCache, SlotObjectCache};

fn loc(group: u64, object: u64) -> Location { Location { group, object } }

fn packed() -> Box<dyn ObjectCache> { Box::new(PackedObjectCache::new(4, 64, 64)) }
fn slot()   -> Box<dyn ObjectCache> { Box::new(SlotObjectCache::new(4, 64)) }
fn level()  -> Box<dyn ObjectCache> { Box::new(LevelSlotObjectCache::new(vec![Level::new(64, 4)])) }

#[rstest]
fn empty(#[values(packed(), slot())] cache: Box<dyn ObjectCache>) {
    assert!(cache.is_empty());
    assert!(cache.latest().is_none());
    assert!(cache.find(loc(0, 0)).is_none());
    assert!(cache.next_after(None).is_none());
}

#[rstest]
fn partial_object(#[values(packed(), slot())] mut cache: Box<dyn ObjectCache>) {
    cache.begin(NewObject::new(loc(0, 0), 10)).unwrap();
    assert_eq!(cache.latest().unwrap().state(), ObjectState::Progressing);
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
    assert_eq!(cache.latest().unwrap().state(), ObjectState::Completed);
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
        cache.begin(NewObject::new(loc(0, i), 1)).unwrap();
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
        cache.begin(NewObject::new(loc(0, i), 1)).unwrap();
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
    cache.begin(NewObject::new(loc(0, 0), 4)).unwrap();
    {
        let mut obj = cache.latest_mut().unwrap();
        obj.unfilled().copy_from_slice(&[10, 20, 30, 40]);
        obj.advance_write_pos(4);
    }
    assert_eq!(cache.latest().unwrap().state(), ObjectState::Completed);
    assert_eq!(cache.latest().unwrap().readable_from(0), &[10, 20, 30, 40]);
}

// ── begin_or_join / fold (multi-source merge) ───────────────────────────────

#[test]
fn single_source_fills_whole_object() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    fold(&mut cache, loc(0, 0), 5, b"hello");
    fold(&mut cache, loc(0, 0), 10, b"world");
    let obj = cache.find(loc(0, 0)).unwrap();
    assert_eq!(obj.state(), ObjectState::Completed);
    assert_eq!(obj.readable_from(0), b"helloworld");
}

#[test]
fn second_source_joins_instead_of_erroring() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    let joined = begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10));
    assert_eq!(joined, Ok(vec![]));
}

#[test]
fn mismatched_join_is_rejected() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    let joined = begin_or_join(&mut cache, NewObject::new(loc(0, 0), 5));
    assert!(matches!(joined, Err(JoinError::Mismatch)));
}

#[test]
fn slower_source_bytes_already_covered_are_discarded_not_duplicated() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    // Fast source delivers everything first.
    fold(&mut cache, loc(0, 0), 10, b"helloworld");
    assert_eq!(cache.find(loc(0, 0)).unwrap().state(), ObjectState::Completed);
    // Slow source's (identical, now-redundant) bytes arrive after — must not
    // double-write or corrupt the already-complete object.
    fold(&mut cache, loc(0, 0), 10, b"helloworld");
    let obj = cache.find(loc(0, 0)).unwrap();
    assert_eq!(obj.readable_from(0), b"helloworld");
}

#[test]
fn partial_overlap_writes_only_the_new_tail() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    fold(&mut cache, loc(0, 0), 5, b"hello"); // a: write_pos now 5
    // b's own read cursor starts from 0 too, delivering the same 10-byte
    // object in one chunk — first 5 bytes already covered by a, only the
    // last 5 ("world") are genuinely new.
    fold(&mut cache, loc(0, 0), 10, b"helloworld");
    let obj = cache.find(loc(0, 0)).unwrap();
    assert_eq!(obj.state(), ObjectState::Completed);
    assert_eq!(obj.readable_from(0), b"helloworld");
}

#[test]
fn join_order_independent() {
    // B begins first, A joins — symmetric with the more common "A begins, B
    // joins" case exercised elsewhere.
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 4)).ok().unwrap();
    let joined = begin_or_join(&mut cache, NewObject::new(loc(0, 0), 4));
    assert_eq!(joined, Ok(vec![]));
    fold(&mut cache, loc(0, 0), 4, b"data");
    assert_eq!(cache.find(loc(0, 0)).unwrap().state(), ObjectState::Completed);
}

#[test]
fn fold_after_eviction_is_a_harmless_no_op() {
    let mut cache = PackedObjectCache::new(1, 64, 64); // capacity 1: next begin() evicts
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 10)).ok().unwrap();
    // Someone else's object evicts loc(0,0) before this source finishes it.
    begin_or_join(&mut cache, NewObject::new(loc(0, 1), 10)).ok().unwrap();
    assert!(cache.find(loc(0, 0)).is_none());
    fold(&mut cache, loc(0, 0), 10, b"helloworld"); // must not panic
}

/// A chunk starting past the write position, e.g. from a source still
/// delivering a `loc` that was evicted and begun again, is not written.
#[test]
fn fold_ignores_chunk_after_gap() {
    let mut cache = PackedObjectCache::new(2, 8, 8);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 6)).ok().unwrap();
    fold(&mut cache, loc(0, 0), 6, b"def");
    assert_eq!(cache.find(loc(0, 0)).unwrap().write_pos(), 0);
    fold(&mut cache, loc(0, 0), 3, b"abc");
    fold(&mut cache, loc(0, 0), 6, b"def");
    assert_eq!(cache.find(loc(0, 0)).unwrap().readable_from(0), b"abcdef");
}

/// `generation()` must bump on every successful `begin()`, and only that —
/// not on a rejected `begin()`, nor on writes to an already-begun object
/// (`find_mut`/`mark_end_of_subgroup`), since those don't add a new object
/// for a scanner to be told about. Same behavior across all three impls.
#[rstest]
fn generation_bumps_only_on_successful_begin(#[values(packed(), slot(), level())] mut cache: Box<dyn ObjectCache>) {
    assert_eq!(cache.generation(), 0);
    cache.begin(NewObject::new(loc(0, 0), 4)).unwrap();
    assert_eq!(cache.generation(), 1);
    cache.begin(NewObject::new(loc(0, 1), 4)).unwrap();
    assert_eq!(cache.generation(), 2);

    // Every impl rejects this: a location that doesn't advance past what's
    // already cached (exact duplicate included).
    assert!(cache.begin(NewObject::new(loc(0, 0), 4)).is_err());
    assert_eq!(cache.generation(), 2);

    {
        let mut o = cache.find_mut(loc(0, 1)).unwrap();
        o.unfilled()[0] = 1;
        o.advance_write_pos(1);
    }
    cache.mark_end_of_subgroup(loc(0, 1));
    assert_eq!(cache.generation(), 2);
}

#[test]
fn join_restarts_canceled_object() {
    let mut cache = PackedObjectCache::new(4, 64, 64);
    begin_or_join(&mut cache, NewObject::new(loc(0, 0), 4)).unwrap();
    fold(&mut cache, loc(0, 0), 2, b"ab");
    let mut obj = cache.find_mut(loc(0, 0)).unwrap();
    obj.set_subgroup_reset();
    obj.cancel();

    assert_eq!(begin_or_join(&mut cache, NewObject::new(loc(0, 0), 4)), Ok(vec![]));
    let obj = cache.find(loc(0, 0)).unwrap();
    assert_eq!(obj.state(), ObjectState::Progressing);
    assert_eq!(obj.write_pos(), 0);
    assert!(!obj.subgroup_reset());
    fold(&mut cache, loc(0, 0), 4, b"abcd");
    assert_eq!(cache.find(loc(0, 0)).unwrap().state(), ObjectState::Completed);
}

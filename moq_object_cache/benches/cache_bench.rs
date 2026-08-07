use criterion::{Criterion, criterion_group, criterion_main};
use moq_object_cache::{Level, LevelSlotObjectCache, ObjectCache, PackedObjectCache, SlotObjectCache};
use moq_wire::{KeyValuePairs, Location};

fn loc(group: u64, object: u64) -> Location {
    Location { group, object }
}

fn write_object(cache: &mut dyn ObjectCache, seq: u64, payload_len: usize) {
    let l = loc(seq / 256, seq % 256);
    cache.begin(l, KeyValuePairs::new(), payload_len).ok();
    if let Some(mut obj) = cache.latest_mut() {
        let buf = obj.unfilled();
        buf.fill(0xAB);
        let n = buf.len();
        obj.advance_write_pos(n);
    }
}

fn read_one(cache: &dyn ObjectCache, after: Option<Location>) -> Option<Location> {
    cache.next_after(after).map(|obj| {
        let s = obj.readable_from(0);
        std::hint::black_box(s.first());
        std::hint::black_box(s.last());
        obj.location()
    })
}

fn fill_cache(cache: &mut dyn ObjectCache, n_objects: usize, sizes: &[usize]) {
    for i in 0..n_objects {
        write_object(cache, i as u64, sizes[i % sizes.len()]);
    }
}

/// One criterion iteration: write one object (evicting oldest), advance consumer one step.
fn steady_state_tick(cache: &mut dyn ObjectCache, seq: &mut u64, read_loc: &mut Option<Location>, sizes: &[usize]) {
    write_object(cache, *seq, sizes[*seq as usize % sizes.len()]);
    *seq += 1;
    *read_loc = read_one(cache, *read_loc);
}

/// Steady-state locality: cache stays full, each iteration writes one object
/// (evicting the oldest) and advances the consumer cursor one step.
fn bench_steady_state(c: &mut Criterion) {
    const MAX_CACHED: usize = 64;

    // Equal-size: 1 KiB audio frames.
    {
        const OBJ_SIZE: usize = 1024;
        let sizes = &[OBJ_SIZE][..];
        let mut group = c.benchmark_group("steady_state_equal_size");

        group.bench_function("SlotObjectCache", |b| {
            let mut cache = SlotObjectCache::new(MAX_CACHED, OBJ_SIZE);
            fill_cache(&mut cache, MAX_CACHED, sizes);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, sizes));
        });
        group.bench_function("PackedObjectCache", |b| {
            let mut cache = PackedObjectCache::new(MAX_CACHED, OBJ_SIZE, OBJ_SIZE);
            fill_cache(&mut cache, MAX_CACHED, sizes);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, sizes));
        });

        group.bench_function("LevelSlotObjectCache", |b| {
            let mut cache = LevelSlotObjectCache::new(vec![Level::new(OBJ_SIZE, MAX_CACHED)]);
            fill_cache(&mut cache, MAX_CACHED, sizes);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, sizes));
        });

        group.finish();
    }

    // Video: I-frame (50 KiB) every 30 P-frames (5 KiB).
    {
        const I_FRAME: usize = 50 * 1024;
        const P_FRAME: usize = 5 * 1024;
        const MAX_OBJ: usize = I_FRAME;
        const AVG_SIZE: usize = (I_FRAME + 29 * P_FRAME) / 30;
        let pattern: Vec<usize> = std::iter::once(I_FRAME)
            .chain(std::iter::repeat(P_FRAME).take(29))
            .collect();

        let mut group = c.benchmark_group("steady_state_video");

        group.bench_function("SlotObjectCache", |b| {
            let mut cache = SlotObjectCache::new(MAX_CACHED, MAX_OBJ);
            fill_cache(&mut cache, MAX_CACHED, &pattern);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, &pattern));
        });
        group.bench_function("PackedObjectCache", |b| {
            let mut cache = PackedObjectCache::new(MAX_CACHED, MAX_OBJ, AVG_SIZE);
            fill_cache(&mut cache, MAX_CACHED, &pattern);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, &pattern));
        });

        // LevelSlotObjectCache: 60 small slots + 4 large slots = 64 total,
        // matching the 1:30 I/P ratio of the pattern (2 I-frames per 60 P-frames).
        group.bench_function("LevelSlotObjectCache", |b| {
            let mut cache = LevelSlotObjectCache::new(vec![
                Level::new(P_FRAME, 60),
                Level::new(I_FRAME, 4),
            ]);
            fill_cache(&mut cache, MAX_CACHED, &pattern);
            let mut seq = MAX_CACHED as u64;
            let mut read_loc = None;
            b.iter(|| steady_state_tick(&mut cache, &mut seq, &mut read_loc, &pattern));
        });

        group.finish();
    }
}

criterion_group!(benches, bench_steady_state);
criterion_main!(benches);

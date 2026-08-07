# moq_object_cache

In-memory caches for reassembling and storing MoQ objects as they arrive
across multiple stream reads, keyed by group/object location.

Several cache shapes are provided (`PackedObjectCache`, `SlotObjectCache`,
`LevelSlotObjectCache`) trading off memory layout and eviction policy
differently — `moq_core` deliberately leaves buffering policy to the
application, and this crate is where that policy lives for callers that want
one of these ready-made instead of rolling their own (e.g.
`moq_core_async::BufferedTrack`, `moq_cli`'s relay).

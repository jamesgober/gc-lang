<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>gc-lang</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

### Added

### Changed

### Fixed

### Security

---

## [1.0.1] - 2026-10-08

A patch for two collector bugs. No public API change: every exported item and
signature is identical to 1.0.0.

### Fixed

- **Stale handles could become valid again after `2^32` reuses of a slot** (ISSUES M60).
  The slot generation advanced with `wrapping_add`, and the free list hands out the
  most recently freed slot first, so an allocate/collect loop reuses one slot every
  cycle; after `2^32` reuses the generation wrapped to a value an old handle still
  carried, and that stale handle resolved to the slot's new occupant (ABA). A slot is
  now **retired** when its last generation (`u32::MAX`) is collected: it stays empty and
  is never returned to the free list, so no `(index, generation)` pair is ever issued
  twice and a stale handle stays stale for the heap's whole lifetime. Capacity
  consequences, documented exactly in `docs/API.md#slot-lifetime`: a retired slot keeps
  its memory and is never reused; a steady allocate/collect loop grows the store by one
  slot per `2^32` reuses of a slot; retired slots count toward the `u32::MAX + 1`
  slot-index ceiling behind `GcError::CapacityExhausted` (about `2^64` allocations to
  reach through retirement alone).
- **A panicking destructor during `collect` leaked its slot and skewed `len`** (ISSUES
  M61, patch part). The sweep dropped the object before doing the slot's bookkeeping
  and subtracted the freed count only after the loop, so a panic in `T::drop` left the
  slot empty but never on the free list (leaked for good) and left `len` too high by
  every object freed so far in that pass, permanently. The sweep now finishes each
  slot's bookkeeping (take the object out, advance or retire the generation, free list,
  live count) before running the destructor, so the unwind leaves through a consistent
  heap: slots swept so far are fully reclaimed, later slots are untouched and reclaimed
  by the next collection. A panic in `Trace::trace` during mark already changed nothing;
  `collect` now also keeps its pooled mark queue in place when user code panics instead
  of losing the allocation.

### Changed

- Documented that `Tracer::mark` accepts a handle of any type and reads it as a position
  in the heap being collected (ISSUES M61): a handle from another heap does not keep its
  own object alive and may keep an unrelated object alive. 1.0.0's rustdoc and
  `docs/API.md` said such a handle was "validated against its own heap", which was
  wrong. Also documented that `Gc<T>` carries no heap identity, so a handle from
  another heap of the same `T` is not detected by `get`/`get_mut`/`contains`. Behaviour
  is unchanged and pinned by `tests/cross_heap.rs`. Typing `mark`, a heap id on handles,
  automatic collection triggers, and weak handles are recorded in `dev/ROADMAP.md` for
  2.0.
- `alloc`, `try_alloc`, `capacity`, `GcError::CapacityExhausted`, and the `Heap`/`Gc`
  type docs now describe slot retirement; `collect` gained a `# Panics` section.

### Added

- Unit tests for slot retirement (last generation, retired slot rejects every
  generation, a hot-slot loop across retirement, exactly one fresh slot per
  retirement), reached through a `#[cfg(test)]`-only hook that moves a free slot's
  generation forward. Not part of the public API.
- A property test: across random allocations, collections (with stale roots), and
  jumps of free slots to the end of their generation range, no handle is issued twice,
  no stale handle resolves, and the free list never leaks a slot.
- `tests/panic_safety.rs` and unit tests: panicking destructors (including one in a
  slot's last generation) and panicking `trace` leave `len` exact and every slot
  reusable.
- `tests/cross_heap.rs`: pins the documented cross-heap handle behaviour.
- A sweep-heavy `collect_unreachable_tree` Criterion group (10k / 100k / 1M objects).

---

## [1.0.0] - 2026-07-01

**API freeze.** The public surface introduced in 0.2.0 is now stable and will not break
until 2.0; 1.x releases are additive only. No functional changes since 0.2.0 — this
release records the SemVer promise and marks the collector production-ready.

### Changed

- `docs/API.md` marked stable with an explicit [SemVer promise](./docs/API.md#stability):
  the six exported items (`Heap`, `Gc`, `Trace`, `Tracer`, `CollectStats`, `GcError`)
  and their documented behaviour are frozen for the `1.x` series.
- Version banners and installation snippets across `README.md` and `docs/API.md` updated
  to `1.0`.

---

## [0.2.0] - 2026-07-01

The core release: a working garbage collector. `gc-lang` now provides a safe,
handle-based tracing mark-and-sweep heap for interpreted-language runtimes. Cycles
are reclaimed, freed slots are reused, and steady-state collection allocates nothing.
The crate has no runtime dependencies and builds `no_std` (needs only `alloc`).

### Added

- `Heap<T>` — the garbage-collected object store: `new`, `with_capacity`, `alloc`,
  `try_alloc`, `get`, `get_mut`, `contains`, `len`, `is_empty`, `capacity`, and
  `collect`, plus `Default` and a shape-only `Debug`.
- `Gc<T>` — an eight-byte, `Copy`, generation-stamped handle. `Eq`, `Ord`, and `Hash`
  for every `T`; a handle to a collected object resolves to `None` rather than
  dangling.
- `Trace` and `Tracer` — the reachability contract. An object reports the handles it
  owns via `Tracer::mark`; the collector follows them during the mark phase.
- `CollectStats` — the `live` / `freed` result of a collection.
- `GcError` — the `#[non_exhaustive]` fallible-allocation error (`CapacityExhausted`).
- Property tests checking reachability soundness against an independent breadth-first
  walk over arbitrary graphs, including cycles and shared subgraphs.
- Criterion benchmarks for allocation (fresh and reused slot), handle resolution,
  a single collection over a reachable tree, and a steady-state allocate/collect loop.
- Runnable examples: `cycle_collection`, `mini_interpreter`, `object_graph`.

### Changed

- Manifest: dropped the unused `serde` optional dependency and `loom` dev-dependency;
  the collector is self-contained on `alloc`. Fixed invalid TOML in `keywords` /
  `categories`. Aligned `clippy.toml` `msrv` with the declared `rust-version` (1.85).
- Adopted the full REPS crate-level lint set (`forbid(unsafe_code)`, `deny(missing_docs)`,
  the `unused_*` denials, and the strict clippy group), matching the sibling crates.

---

## [0.1.0] - 2026-06-18

Initial scaffold and repository bootstrap. No domain logic yet &mdash; this release establishes the structure, tooling, and quality gates the implementation will be built on.

### Added

- `Cargo.toml` with crate metadata, Rust 2024 edition, MSRV 1.85.
- Dual `Apache-2.0 OR MIT` license files.
- `README.md`, `CHANGELOG.md`, and a documentation skeleton.
- `REPS.md` compliance baseline.
- `.github/workflows/ci.yml` CI matrix; `deny.toml`, `clippy.toml`, `rustfmt.toml`.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md` (committed engineering standards + plan).

[Unreleased]: https://github.com/jamesgober/gc-lang/compare/v1.0.1...HEAD
[1.0.1]: https://github.com/jamesgober/gc-lang/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/jamesgober/gc-lang/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/gc-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/gc-lang/releases/tag/v0.1.0

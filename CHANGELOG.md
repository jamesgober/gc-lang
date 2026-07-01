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

[Unreleased]: https://github.com/jamesgober/gc-lang/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/gc-lang/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/gc-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/gc-lang/releases/tag/v0.1.0

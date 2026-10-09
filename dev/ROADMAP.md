# gc-lang - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Master plan: ../../_strategy/LANG_COLLECTION.md
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt.

## v0.2.0 - Core (THE HARD PART, NOT DEFERRED) — DONE
A garbage collector for the LexerSketch runtime (interpreted languages).
Delivered a safe, handle-based tracing mark-and-sweep collector: `Heap<T>`, the
`Gc<T>` generation-stamped handle, the `Trace`/`Tracer` reachability contract, and
`GcError`. Cycles are reclaimed; slots are reused; scratch is pooled.
Exit criteria:
- [x] Every public item has rustdoc + a runnable example.
- [x] Core invariants property-tested (reachability soundness vs. an independent BFS).

**Dependency note (anti-deferral rule):** the roadmap anticipated wiring
`arena-lang` here. It was evaluated and deliberately not wired. `arena-lang` is
append-only — it never reclaims an individual slot — whereas a collector's whole
job is to reclaim and reuse slots. Forcing an append-only arena under a reclaiming
collector would defeat the point, so the heap owns its own slot store with a free
list. No hard task was deferred; the dependency simply was not a fit for the design.

## v1.0.0 - API freeze — DONE
Public surface stable and frozen until 2.0. No new API: the minimal 0.2.0 surface
(`Heap`, `Gc`, `Trace`, `Tracer`, `CollectStats`, `GcError`) is promoted to stable
as-is. 1.x is additive only.
- [x] docs/API.md marked stable; SemVer promise recorded (docs/API.md#stability).
- [x] Full test + benchmark suite green on all three platforms (CI matrix:
      Linux/macOS/Windows × stable/1.85; verified locally on WSL2 Ubuntu).

## v1.0.1 - Patch: generation retirement and panic-safe sweep — DONE
Fixes ISSUES M60 and the patch-level parts of M61. No public API change; every
exported item and signature is identical to 1.0.0.

Delivered:
- **M60:** slot generations no longer wrap. The sweep advances a slot's generation
  with `checked_add`; when there is no next generation (the occupant at `u32::MAX`
  was collected) the slot is retired — left empty at `u32::MAX` and never returned
  to the free list. No `(index, generation)` pair is ever issued twice, so a stale
  handle stays stale for the heap's lifetime. Capacity behaviour is documented
  exactly (docs/API.md#slot-lifetime): a retired slot keeps its memory; a steady
  loop grows the store by one slot per `2^32` reuses of a slot; retired slots count
  toward the `u32::MAX + 1` index ceiling (~`2^64` allocations via retirement).
  Tested through a `#[cfg(test)]`-only hook that moves a free slot's generation
  forward, plus a property test (random alloc / collect-with-stale-roots / jump
  sequences) that no handle is issued twice, no stale handle resolves, and no slot
  leaks.
- **M61 (patch parts):** the sweep finishes each slot's bookkeeping (take the
  object out, advance or retire the generation, free list, live count) *before* the
  destructor runs, so a panicking `T::drop` unwinds out of a consistent heap: no
  leaked slot, exact `len`; unswept garbage is reclaimed by the next collection.
  `collect` works on its pooled fields in place, so a panic in user code no longer
  loses the pooled mark queue. Tested with panicking destructors and `trace` under
  `catch_unwind` (unit tests + `tests/panic_safety.rs`; the latter fails on 1.0.0).
- **M61 (docs):** documented that `Tracer::mark` accepts any `Gc<U>` and reads it as
  a position in the collecting heap (a foreign handle does not keep its own object
  alive and may over-retain an unrelated one), and that `Gc<T>` carries no heap
  identity. Corrected the 1.0.0 claim that a foreign handle "is validated against its
  own heap". Behaviour pinned by `tests/cross_heap.rs`.

Dependency wiring: none. Still zero runtime dependencies; no first-party crate
wired (unchanged from 1.0.0).

## 2.0 - planned breaking changes (recorded per the anti-deferral rule)
These change the public surface or `Gc<T>`'s layout, so they cannot ship in 1.x.
None is held by difficulty; each is held only by SemVer, and the LexerSketch
roadmap (phase 4.5) schedules them together with wiring gc-lang into value-lang /
bvm-lang so the 2.0 surface is designed once.
- **Typed `mark` (M61).** `Tracer::mark<U>(Gc<U>)` accepts a handle of any type.
  2.0 makes the tracer carry the heap's element type (e.g. `Tracer<'_, T>` with
  `mark(Gc<T>)`) so marking a handle of the wrong type is a compile error. Changing
  the `Trace` / `Tracer` signatures breaks every `Trace` impl, hence 2.0.
- **Heap id on handles (M61).** `Gc<T>` carries no heap identity, so a handle from
  another `Heap<T>` of the same `T` is not detected. Options for 2.0: a per-heap id
  stamped into handles (grows `Gc` past 8 bytes, or steals generation bits), or a
  debug-build-only id checked by `get`/`collect`. Either changes `Gc`'s size or
  semantics that 1.x documents (8 bytes), so it waits for 2.0.
- **Automatic collection triggers.** 1.x never collects on its own. A runtime-facing
  trigger (an allocation-count or live-growth threshold that asks the runtime for
  its roots, e.g. through a root-provider trait) is planned with the value-lang /
  bvm-lang wiring. A purely additive form (a policy query such as "should collect
  now?") could land in 1.x first if the VM work needs it before 2.0.
- **Weak handles.** A handle that does not keep its object alive and is cleared or
  reported when the object is collected (for caches, interning tables, and
  finalization). Note that an *unmarked* `Gc` already behaves as a weak reference
  that resolves to `None` after collection, and since 1.0.1 that holds for the heap's
  lifetime; a dedicated weak type adds notification/clearing, planned with 2.0.

## Beyond 1.0 (additive, non-breaking)
Candidate 1.x additions, none required for the freeze: `Heap::clear`, live-object
iteration, incremental/generational collection modes, per-collection byte accounting,
a retired-slot count in `Debug` / stats.

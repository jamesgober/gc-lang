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

## Beyond 1.0 (additive, non-breaking)
Candidate 1.x additions, none required for the freeze: `Heap::clear`, live-object
iteration, incremental/generational collection modes, per-collection byte accounting.

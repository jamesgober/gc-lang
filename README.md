<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>gc-lang</b>
    <br>
    <sub><sup>GARBAGE COLLECTOR</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/gc-lang"><img alt="Crates.io" src="https://img.shields.io/crates/v/gc-lang"></a>
    <a href="https://crates.io/crates/gc-lang"><img alt="Downloads" src="https://img.shields.io/crates/d/gc-lang?color=%230099ff"></a>
    <a href="https://docs.rs/gc-lang"><img alt="docs.rs" src="https://img.shields.io/docsrs/gc-lang"></a>
    <a href="https://github.com/jamesgober/gc-lang/actions"><img alt="CI" src="https://github.com/jamesgober/gc-lang/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        gc-lang is the LXRT-tier crate: A garbage collector for the LexerSketch runtime (interpreted languages). Part of the -lang language-construction family; see _strategy/LANG_COLLECTION.md for the master plan.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition).
    </p>
    <blockquote>
        <strong>1.0.0 is the API freeze.</strong> The public surface is stable and will not break until <code>2.0</code>. See <a href="./CHANGELOG.md"><code>CHANGELOG.md</code></a> and the <a href="./docs/API.md#stability">SemVer promise</a>.
    </blockquote>
</div>

<hr>
<br>

## Overview

`gc-lang` gives an interpreted-language runtime one thing, done carefully: a heap it
can allocate objects into and periodically sweep, reclaiming everything no longer
reachable — **cycles included**. Allocate a value into a `Heap<T>` and hold the
returned `Gc<T>` handle; objects store handles to point at one another; when the
runtime hands its roots to `collect`, the collector traces the reachable graph and
frees the rest.

It is a **tracing mark-and-sweep** collector, and it is **entirely safe** — the crate
is `#![forbid(unsafe_code)]`. Two design choices carry that:

- **Reachability, not reference counting.** What lives is decided by tracing from
  roots, so unreachable cycles are reclaimed. A runtime can build arbitrary object
  graphs — back-edges, doubly-linked structures, self-references — without leaking.
- **Handles, not pointers.** A `Gc<T>` is a slot index plus a generation stamp, not
  an address. Objects wire to each other by handle, so the graph never fights the
  borrow checker, and a handle to a collected object resolves to `None` instead of
  dangling. There is no use-after-free to have.

It owns object storage and reclamation only — no value representation, no interpreter,
no policy on *when* to collect. The runtime keeps that control.

<br>

## Features

- **Cycle-collecting** — tracing reachability reclaims what reference counting cannot.
- **Safe by construction** — `#![forbid(unsafe_code)]`; a stale handle reads as absent, never dangles.
- **Generational handles** — `Gc<T>` is `Copy`, eight bytes, and `Eq`/`Ord`/`Hash` for any `T`.
- **Slot reuse** — sweeping returns slots to a free list; steady-state loops never grow the store.
- **Allocation-free collection** — the mark queue and mark bitset are pooled across passes.
- **`no_std`** — needs only `alloc`; the default `std` feature changes nothing in the public surface.
- **Zero runtime dependencies** — the collector is self-contained.

<br>
<hr>

## Installation

```toml
[dependencies]
gc-lang = "1.0"
```

Or from the terminal:

```bash
cargo add gc-lang
```

`no_std` (needs a global allocator in your target):

```toml
[dependencies]
gc-lang = { version = "1.0", default-features = false }
```

<br>

## Quick Start

```rust
use gc_lang::{Gc, Heap, Trace, Tracer};

// The runtime's value type. Compound variants own handles to other values;
// `trace` reports them so the collector can follow them.
enum Value {
    Number(f64),
    Pair(Gc<Value>, Gc<Value>),
}

impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        if let Value::Pair(a, b) = self {
            tracer.mark(*a);
            tracer.mark(*b);
        }
    }
}

let mut heap = Heap::new();
let one = heap.alloc(Value::Number(1.0));
let two = heap.alloc(Value::Number(2.0));
let pair = heap.alloc(Value::Pair(one, two));
let unused = heap.alloc(Value::Number(3.0));

// Collect with `pair` as the only root: `pair`, `one`, `two` survive; `unused` does not.
let stats = heap.collect([pair]);
assert_eq!(stats.freed, 1);
assert!(heap.get(unused).is_none());
assert!(heap.get(one).is_some());
```

<br>

## How It Works

A collection is two phases:

1. **Mark.** Starting from each root handle, the collector calls `Trace::trace` on
   every object it reaches and follows the handles that object reports. Each object is
   visited once, so cycles terminate and shared subgraphs are not re-scanned. Marks
   live in a packed bitset, one bit per slot.
2. **Sweep.** Every object not marked is dropped, its slot's generation is advanced —
   which invalidates any outstanding handle to it — and the slot is returned to a free
   list for the next allocation to reuse.

The generation stamp is the safety mechanism: because a reused slot carries a new
generation, a handle to a collected object no longer matches whatever now lives there.
It resolves to `None`, never to an unrelated value.

The cost is `O(reachable)` to mark plus `O(slots)` to sweep. The mark queue and the
mark bitset are retained between calls, so a steady-state collection allocates nothing.

<br>

## Examples

Runnable examples live in [`examples/`](./examples):

```bash
cargo run --example cycle_collection   # reclaim a reference cycle reference-counting can't
cargo run --example mini_interpreter   # a GC'd value heap rooted at an operand stack
cargo run --example object_graph       # shared subgraphs and a shrinking root set
```

<br>

## Performance

Latest local Criterion means (release build, WSL2 Ubuntu on Windows x86_64). Numbers
vary by CPU and environment; reproduce with `cargo bench`.

| Operation                         | Cost        | Notes                                    |
|-----------------------------------|-------------|------------------------------------------|
| Handle resolution (`get`)         | ~0.6 ns     | direct slot lookup + generation check    |
| Allocation, reused slot           | ~2.3 ns     | free-list fast path                      |
| Allocation, fresh slot            | ~6.7 ns     | grows the backing store                  |
| Collection                        | ~12 ns/obj  | linear in reachable + swept objects      |
| Steady-state alloc + collect      | ~28 ns      | per 3-object allocate-then-reclaim cycle |

Collection scales linearly with heap size: ~10.8 µs for 1,000 reachable nodes,
~1.24 ms for 100,000.

<br>

## Cross-Platform

Pure safe Rust on `alloc`, with no platform-specific code paths. The full suite runs
on Linux, macOS, and Windows (x86_64 and ARM64) across the CI matrix on stable and the
1.85 MSRV.

<br>

## Testing

```bash
cargo test --all-features          # unit + property + doc tests
cargo test --no-default-features   # no_std build
cargo bench                        # Criterion benchmarks
```

The property suite checks the collector's core invariant — an object is live after a
collection **if and only if** it was reachable from a root — against an independent
breadth-first walk over arbitrary generated graphs, cycles and shared subgraphs
included.

<br>

## Status

**Stable — 1.0.0 is the API freeze.** The public surface is frozen and will not break
until `2.0`; 1.x releases are additive only. See the
[SemVer promise](./docs/API.md#stability), [`docs/API.md`](./docs/API.md), the
[`ROADMAP`](./dev/ROADMAP.md), and [`CHANGELOG.md`](./CHANGELOG.md).

<br>

## Contributing

Engineering standards live in [`REPS.md`](./REPS.md); the phase plan is in
[`dev/ROADMAP.md`](./dev/ROADMAP.md). Before a PR, all of the following must be clean:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober <me@jamesgober.com>.</strong></sup>
</div>

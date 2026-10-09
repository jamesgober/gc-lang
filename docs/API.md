<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>gc-lang</b><br>
    <sub><sup>API REFERENCE</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>API</span>
        <span>&nbsp;│&nbsp;</span>
        <a href="../CHANGELOG.md" title="Changelog"><b>CHANGELOG</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="../dev/ROADMAP.md" title="Roadmap"><b>ROADMAP</b></a>
    </sup>
</div>
<br>

> **Version 1.0.1 — stable.** The public surface documented here is frozen and will not
> break until `2.0`; see the [SemVer promise](#stability). Everything below is live and
> tested. 1.0.1 changed no signature; it fixed generation wrap-around (stale handles now
> stay stale for the heap's lifetime, see [Slot lifetime](#slot-lifetime)) and made
> [`collect`](#heap-collect) panic-safe.

A tracing, cycle-collecting, `#![forbid(unsafe_code)]` garbage collector for
interpreted-language runtimes. Allocate objects into a [`Heap`](#heap), refer to them
by [`Gc`](#gc) handle, and reclaim the unreachable ones with
[`Heap::collect`](#heap-collect).

<br>

## Table of Contents

- **[Installation](#installation)**
- **[Concepts](#concepts)**
- **[Quick Start](#quick-start)**
- **[Public API](#public-api)**
  - [`Heap<T>`](#heap)
    - [`new` / `with_capacity`](#heap-new)
    - [`alloc` / `try_alloc`](#heap-alloc)
    - [`get` / `get_mut` / `contains`](#heap-get)
    - [`len` / `is_empty` / `capacity`](#heap-len)
    - [`collect`](#heap-collect)
    - [Slot lifetime and capacity](#slot-lifetime)
  - [`Gc<T>`](#gc)
  - [`Trace`](#trace)
  - [`Tracer`](#tracer)
  - [`CollectStats`](#collectstats)
  - [`GcError`](#gcerror)
- **[Usage Patterns](#usage-patterns)**
  - [Choosing roots](#pattern-roots)
  - [When to collect](#pattern-when)
  - [Multiple heaps](#pattern-multiple-heaps)
  - [`no_std`](#pattern-no-std)
- **[Stability & SemVer Promise](#stability)**
- **[API Safety](#api-safety)**

<br><br>

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
gc-lang = "1.0"
```

Or via the terminal:

```bash
cargo add gc-lang
```

`no_std` (needs a global allocator on your target):

```toml
[dependencies]
gc-lang = { version = "1.0", default-features = false }
```

<hr>
<br>

<h2 id="concepts">Concepts</h2>

The collector rests on three ideas.

**The heap owns the objects.** A [`Heap<T>`](#heap) is a store of `T` values. It hands
back a [`Gc<T>`](#gc) handle for each allocation and reclaims objects when you ask it
to. A program can run several heaps, of the same or different `T`.

**Handles refer; they do not own.** A [`Gc<T>`](#gc) is a slot index plus a generation
stamp — eight bytes, `Copy`. Objects store handles to refer to one another. Because a
handle is not a borrow, an object graph can point in every direction without the borrow
checker's involvement; because it carries a generation, a handle to a collected object
resolves to `None` rather than dangling.

**Tracing decides what lives.** You call [`collect`](#heap-collect) with a set of
*roots* — the handles your runtime considers live from the outside. The collector
marks everything reachable from the roots by walking the handles each object reports
through [`Trace`](#trace), then sweeps everything it did not mark. Unreachable cycles
are reclaimed, because reachability — not a reference count — is the test.

<hr>
<br>

<h2 id="quick-start">Quick Start</h2>

```rust
use gc_lang::{Gc, Heap, Trace, Tracer};

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

let stats = heap.collect([pair]);   // root: pair
assert_eq!(stats.freed, 1);         // unused was unreachable
assert!(heap.get(one).is_some());   // reachable through pair
```

<hr>
<br>

<h2 id="public-api">Public API</h2>

The crate exports six items: [`Heap`](#heap), [`Gc`](#gc), [`Trace`](#trace),
[`Tracer`](#tracer), [`CollectStats`](#collectstats), and [`GcError`](#gcerror).

<br>

<h3 id="heap"><code>Heap&lt;T&gt;</code></h3>

Source: `src/heap.rs`.

The garbage-collected object store. `T` is the runtime's object type. Allocation and
resolution work for any `T`; [`collect`](#heap-collect) additionally requires
`T: Trace` so the collector can follow each object's handles.

`Heap<T>` is `Send`/`Sync` when `T` is, `Default` (equivalent to `new`), and has a
shape-only `Debug` (live count, free count, capacity — never the contents, so there is
no `T: Debug` bound).

<br>

<h4 id="heap-new">Construction — <code>new</code>, <code>with_capacity</code></h4>

- `const fn new() -> Heap<T>`
  Creates an empty heap. Allocates nothing until the first object is added. Being
  `const`, it can initialise a `static`.
- `fn with_capacity(capacity: usize) -> Heap<T>`
  Creates an empty heap with room for `capacity` objects preallocated. A hint only:
  it reserves backing storage so the first `capacity` allocations do not reallocate.
  Size it to the runtime's expected live-object count.

**Parameters:** `capacity` — the number of object slots to preallocate.

```rust
use gc_lang::Heap;

// Empty; no allocation yet.
let a: Heap<u32> = Heap::new();
assert!(a.is_empty());

// Preallocated for 1024 objects.
let b: Heap<u64> = Heap::with_capacity(1024);
assert!(b.capacity() >= 1024);
```

<br>

<h4 id="heap-alloc">Allocation — <code>alloc</code>, <code>try_alloc</code></h4>

- `fn alloc(&mut self, value: T) -> Gc<T>`
  Allocates `value` and returns a handle to it. Reuses a slot freed by an earlier
  collection when one is available, otherwise grows the store. **Panics** only if the
  heap has exhausted its slot space: no slot is free and all `u32::MAX + 1` slot
  indices are occupied or retired (see [Slot lifetime](#slot-lifetime) — on the order
  of `2^64` allocations for a heap that collects).
- `fn try_alloc(&mut self, value: T) -> Result<Gc<T>, GcError>`
  The non-panicking counterpart. Returns [`GcError::CapacityExhausted`](#gcerror) at
  the slot-space ceiling instead of panicking; the heap is left unchanged. Prefer it
  when allocating in response to untrusted input whose volume you do not control.

**Parameters:** `value` — the object to store, moved into the heap.

**Returns:** a [`Gc<T>`](#gc) handle valid until the object is collected.

```rust
use gc_lang::{Heap, Trace, Tracer};

struct Obj(u32);
impl Trace for Obj {
    fn trace(&self, _: &mut Tracer<'_>) {}   // leaf: owns no handles
}

let mut heap = Heap::new();

// Infallible form.
let h = heap.alloc(Obj(1));
assert!(heap.get(h).is_some());

// Fallible form — same result, explicit error path.
let h2 = heap.try_alloc(Obj(2))?;
assert!(heap.get(h2).is_some());
# Ok::<(), gc_lang::GcError>(())
```

<br>

<h4 id="heap-get">Resolution — <code>get</code>, <code>get_mut</code>, <code>contains</code></h4>

- `fn get(&self, handle: Gc<T>) -> Option<&T>`
  Borrows the object behind `handle`, or `None` if the handle does not name a live
  object — either out of range or stale (its object was collected and the slot has
  moved to a new generation). A stale handle stays stale for the heap's whole
  lifetime: generations never wrap ([Slot lifetime](#slot-lifetime)).
- `fn get_mut(&mut self, handle: Gc<T>) -> Option<&mut T>`
  The mutating counterpart, with the same staleness guarantee. Use it to update an
  object in place, including rewiring the handles it holds.
- `fn contains(&self, handle: Gc<T>) -> bool`
  `true` if `handle` names a live object; equivalent to `get(handle).is_some()`
  without producing a borrow.

**Parameters:** `handle` — a [`Gc<T>`](#gc) previously returned by this heap. A handle
carries no heap identity, so a handle from a *different* `Heap<T>` of the same `T` is
not detected: if its index and generation match a live object here, it resolves to
that unrelated object. Use each handle only with the heap that returned it.

```rust
use gc_lang::{Heap, Trace, Tracer};

struct Cell(i64);
impl Trace for Cell {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

let mut heap = Heap::new();
let h = heap.alloc(Cell(0));

// Read.
assert_eq!(heap.get(h).map(|c| c.0), Some(0));
assert!(heap.contains(h));

// Mutate in place.
if let Some(cell) = heap.get_mut(h) {
    cell.0 = 42;
}
assert_eq!(heap.get(h).map(|c| c.0), Some(42));
```

A stale handle is safe to resolve — it simply reads as absent:

```rust
# use gc_lang::{Heap, Trace, Tracer};
# struct Cell(i64);
# impl Trace for Cell { fn trace(&self, _: &mut Tracer<'_>) {} }
let mut heap = Heap::new();
let h = heap.alloc(Cell(1));
let _ = heap.collect([]);          // nothing rooted: h's object is reclaimed
assert!(heap.get(h).is_none());    // no dangling, no panic
assert!(!heap.contains(h));
```

<br>

<h4 id="heap-len">Introspection — <code>len</code>, <code>is_empty</code>, <code>capacity</code></h4>

- `fn len(&self) -> usize` — live (occupied-slot) count; freed slots awaiting reuse are
  not counted.
- `fn is_empty(&self) -> bool` — `true` when no live objects remain.
- `fn capacity(&self) -> usize` — slots the backing store can hold before it must grow,
  including free and retired slots. Never decreases across a collection: sweeping
  returns slots to the free list rather than releasing memory, so the store stays sized
  to its high-water mark.

```rust
use gc_lang::{Heap, Trace, Tracer};

struct Leaf;
impl Trace for Leaf {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

let mut heap = Heap::with_capacity(8);
assert!(heap.is_empty());
heap.alloc(Leaf);
heap.alloc(Leaf);
assert_eq!(heap.len(), 2);
assert!(heap.capacity() >= 8);
```

<br>

<h4 id="heap-collect">Collection — <code>collect</code></h4>

- `fn collect<I>(&mut self, roots: I) -> CollectStats where I: IntoIterator<Item = Gc<T>>, T: Trace`

Reclaims every object not reachable from `roots`, returning a [`CollectStats`](#collectstats)
describing the pass. Two phases:

- **Mark:** from each root, follow the handles every object reports through
  [`Trace::trace`](#trace), visiting each reachable object once. Cycles terminate;
  shared subgraphs are not re-scanned.
- **Sweep:** drop every unmarked object, advance its slot's generation (invalidating
  outstanding handles), and return the slot to the free list — or retire it, if that
  was its last generation ([Slot lifetime](#slot-lifetime)).

**Parameters:** `roots` — anything iterable yielding `Gc<T>`: an array (`[a, b]`), a
`Vec` drained with `.iter().copied()`, or any iterator. A stale root is ignored, so a
conservative, slightly-oversized root set is safe.

**Returns:** [`CollectStats`](#collectstats) — `live` survivors and `freed`
reclamations. `live + freed` equals the pre-collection population.

**Cost:** `O(reachable)` to mark, `O(slots)` to sweep. The mark queue and mark bitset
are retained between calls, so a steady-state collection allocates nothing.

**Panics:** `collect` does not panic itself, but it runs your code — `Trace::trace`
during mark, `T`'s destructor during sweep — and a panic there propagates out of
`collect`. The heap stays consistent (since 1.0.1). A panic during mark happens before
anything is swept, so nothing changes. A panic in a destructor leaves every slot swept
up to and including the panicking one fully reclaimed (generation advanced, slot on the
free list, `len` down by one) and every later slot untouched (object still stored,
still counted by `len`); the next collection reclaims what the interrupted one did not
reach. No slot leaks, and `len` always equals the number of objects stored. (In 1.0.0 a
panicking destructor leaked its slot and left `len` too high for the rest of the heap's
life.)

Unreachable cycles are reclaimed — this is the case reference counting cannot handle:

```rust
use gc_lang::{Gc, Heap, Trace, Tracer};

struct Node {
    link: Option<Gc<Node>>,
}
impl Trace for Node {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        if let Some(link) = self.link {
            tracer.mark(link);
        }
    }
}

let mut heap = Heap::new();
let a = heap.alloc(Node { link: None });
let b = heap.alloc(Node { link: Some(a) });
heap.get_mut(a).unwrap().link = Some(b);   // a <-> b, no external reference

let stats = heap.collect([]);              // empty root set
assert_eq!(stats.freed, 2);
assert!(heap.is_empty());
```

Rooting from an interpreter's operand stack:

```rust
use gc_lang::{Gc, Heap, Trace, Tracer};

struct Value(Vec<Gc<Value>>);
impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        for &child in &self.0 {
            tracer.mark(child);
        }
    }
}

let mut heap = Heap::new();
let mut stack: Vec<Gc<Value>> = Vec::new();

let kept = heap.alloc(Value(vec![]));
stack.push(kept);                          // on the stack -> a root
let _scratch = heap.alloc(Value(vec![]));  // not on the stack

let stats = heap.collect(stack.iter().copied());
assert_eq!(stats.live, 1);
assert!(heap.get(kept).is_some());
```

<br>

<h4 id="slot-lifetime">Slot lifetime and capacity</h4>

Each slot carries a 32-bit generation, so it can hold exactly `2^32` occupants over the
heap's lifetime — generations `0` through `u32::MAX`. When the occupant at generation
`u32::MAX` is collected, the slot is **retired**: it stays empty, it is never returned
to the free list, and no handle into it can ever resolve again. The generation never
wraps back to `0`. (In 1.0.0 it did: with a most-recently-freed-first free list, an
allocate/collect loop reuses the same slot every cycle, and after `2^32` reuses a stale
handle named the slot's new occupant. 1.0.1 retires the slot instead.)

What that means for capacity, exactly:

- **A stale handle stays stale for the heap's lifetime.** No `(index, generation)` pair
  is ever issued twice.
- **A retired slot keeps its memory.** It stays in the backing store (one slot's worth
  of `Option<T>` plus a `u32`) and counts toward [`capacity`](#heap-len), but is never
  reused. The store never shrinks, as before.
- **The store grows by one slot per retirement.** When a hot slot retires, the next
  allocation that finds no free slot appends a fresh one. A steady allocate/collect
  loop therefore grows the store by one slot each time one of its slots has been
  reused `2^32` times, and not otherwise.
- **Retired slots count toward the slot-space ceiling.**
  [`GcError::CapacityExhausted`](#gcerror) is returned when no slot is free and all
  `u32::MAX + 1` indices are occupied or retired. Retiring every index takes
  `2^32 × 2^32 = 2^64` allocations. If it ever happens, no collection can recover; the
  heap must be replaced.

<hr>
<br>

<h3 id="gc"><code>Gc&lt;T&gt;</code></h3>

Source: `src/handle.rs`.

A small, `Copy`, type-tagged handle to one object in a [`Heap<T>`](#heap). Eight bytes
— a slot index plus a generation stamp. There is no public constructor; a `Gc` can only
come from [`alloc`](#heap-alloc) / [`try_alloc`](#heap-alloc).

- **Type-tagged:** the `T` is compile-time only and occupies no space. It stops a
  `Gc<Value>` from being resolved against a `Heap<Node>`.
- **No heap identity:** the handle does not record which heap minted it. Two heaps of
  the same `T` cannot tell each other's handles apart; a handle from one may resolve to
  an unrelated object in the other. Use each handle only with the heap that returned
  it. (A heap id on handles is planned for 2.0; see the ROADMAP.)
- **Universally `Copy`/`Eq`/`Ord`/`Hash`:** the tag never adds a bound, so `Gc<T>`
  works as a map key regardless of what it points at.
- **Stale, never dangling:** the generation stamp advances when a slot is reused, so a
  handle to a collected object resolves to `None` — for the heap's whole lifetime,
  since generations never wrap ([Slot lifetime](#slot-lifetime)).

```rust
use std::collections::HashMap;
use gc_lang::{Heap, Trace, Tracer};

struct Node;
impl Trace for Node {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

let mut heap = Heap::new();
let h = heap.alloc(Node);

// Copy, comparable, eight bytes wide whatever it points at.
let also = h;
assert_eq!(h, also);
assert_eq!(core::mem::size_of_val(&h), 8);

// Usable as a map key — e.g. side-tables keyed by object identity.
let mut labels: HashMap<_, &str> = HashMap::new();
labels.insert(h, "root");
assert_eq!(labels.get(&h), Some(&"root"));
```

<hr>
<br>

<h3 id="trace"><code>Trace</code></h3>

Source: `src/trace.rs`.

The contract that makes an object type collectable. During the mark phase the collector
calls `trace` on each object it visits; the object reports every [`Gc`](#gc) handle it
owns by calling [`Tracer::mark`](#tracer).

```rust
fn trace(&self, tracer: &mut Tracer<'_>);
```

The contract:

- **Mark every owned handle.** A handle you hold but never mark is invisible to the
  collector; its object can be swept while you still hold a handle to it. (Not unsound —
  the handle just resolves to `None` afterward — but rarely intended.)
- **Marking extra is safe.** The collector ignores handles that do not name a live
  object, so an over-broad `trace` at worst keeps something alive one cycle longer.
- **`trace` is read-only.** It takes `&self`; it must not mutate the object or allocate
  into the heap.
- **Mark only handles into the heap being collected.** See [`Tracer::mark`](#tracer):
  a handle from another heap does not keep its own object alive and may keep an
  unrelated one alive.

Leaf types implement it as an empty body. A composite reports each field:

```rust
use gc_lang::{Gc, Trace, Tracer};
use std::collections::HashMap;

enum Value {
    Nil,
    Int(i64),
    Pair(Gc<Value>, Gc<Value>),
    List(Vec<Gc<Value>>),
    Record(HashMap<String, Gc<Value>>),
}

impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        match self {
            Value::Nil | Value::Int(_) => {}                    // leaves
            Value::Pair(a, b) => {
                tracer.mark(*a);
                tracer.mark(*b);
            }
            Value::List(items) => {
                for &item in items {
                    tracer.mark(item);
                }
            }
            Value::Record(fields) => {
                for &handle in fields.values() {
                    tracer.mark(handle);
                }
            }
        }
    }
}
```

<hr>
<br>

<h3 id="tracer"><code>Tracer&lt;'a&gt;</code></h3>

Source: `src/trace.rs`.

The sink a [`Trace`](#trace) implementation reports its outgoing edges to. Handed to
`trace` by the collector; you never construct one. Its single method:

- `fn mark<T>(&mut self, handle: Gc<T>)`
  Records that the object being traced holds `handle`, so the collector will visit —
  and keep alive — the object it names. Call it once per owned handle. Recording an edge
  is a single push onto the collector's pooled work queue, so it does not allocate on
  the steady-state path.

**Parameters:** `handle` — a [`Gc<T>`](#gc) this object owns, **into the heap being
collected**.

**Only handles into the collecting heap do anything.** `mark` is generic: it accepts a
`Gc<U>` for any `U`, and the compiler does not check that the handle belongs to the
heap that is collecting. The collector reads every marked handle as an
`(index, generation)` position in *that* heap. A handle from anywhere else (a heap of
another type, or another heap of the same type):

- **does not keep its own object alive.** Marking a handle into heap `B` while heap `A`
  collects does nothing for `B`. An object in `B` referenced only from `A`'s objects
  survives `B`'s collections only if you pass that handle to `B`'s `collect` as a root.
- **may keep an unrelated object alive.** If its index and generation match a live
  object in `A`, that object is marked (and traced) for this collection though nothing
  in `A` references it. Never unsound — no handle can dangle — but it is over-retention
  until a collection in which the coincidence does not recur.

(1.0.0's documentation said a foreign handle was "validated against its own heap"; that
was wrong, and is corrected here. The behaviour itself is unchanged and is pinned by
`tests/cross_heap.rs`.) Restricting `mark` to the collecting heap's handle type is a
breaking change, planned for 2.0.

```rust
use gc_lang::{Gc, Trace, Tracer};

struct Cell {
    next: Option<Gc<Cell>>,
    prev: Option<Gc<Cell>>,   // back-edge: fine, tracing handles it
}

impl Trace for Cell {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        if let Some(next) = self.next {
            tracer.mark(next);
        }
        if let Some(prev) = self.prev {
            tracer.mark(prev);
        }
    }
}
```

<hr>
<br>

<h3 id="collectstats"><code>CollectStats</code></h3>

Source: `src/heap.rs`.

The result of a [`collect`](#heap-collect) pass. `#[non_exhaustive]` — read the fields
you need; construct it only through the collector.

| Field   | Type    | Meaning                                        |
|---------|---------|------------------------------------------------|
| `live`  | `usize` | objects that survived — the reachable set      |
| `freed` | `usize` | objects reclaimed — the unreachable set        |

`live + freed` equals the number of objects resident when the pass began. Derives
`Clone`, `Copy`, `Debug`, `PartialEq`, `Eq`.

```rust
use gc_lang::{Heap, Trace, Tracer};

struct Leaf;
impl Trace for Leaf {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

let mut heap = Heap::new();
let root = heap.alloc(Leaf);
let _dead = heap.alloc(Leaf);

let stats = heap.collect([root]);
assert_eq!(stats.live, 1);
assert_eq!(stats.freed, 1);
```

<hr>
<br>

<h3 id="gcerror"><code>GcError</code></h3>

Source: `src/error.rs`.

The error returned by [`try_alloc`](#heap-alloc). `#[non_exhaustive]`; derives `Clone`,
`Copy`, `Debug`, `PartialEq`, `Eq`, and implements `Display` + `core::error::Error`.

| Variant             | Meaning                                                                 |
|---------------------|-------------------------------------------------------------------------|
| `CapacityExhausted` | No slot is free and every one of the `u32::MAX + 1` slot indices is occupied or retired. |

Unreachable for a heap that collects — it takes more than four billion slots that are
live or never reclaimed, or on the order of `2^64` allocations to retire them all (see
[Slot lifetime](#slot-lifetime)). Recover by running a collection to free dead slots,
then retrying; if every slot has retired, nothing can be reclaimed.

```rust
use gc_lang::{GcError, Heap, Trace, Tracer};

struct Leaf;
impl Trace for Leaf {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

let mut heap: Heap<Leaf> = Heap::new();
match heap.try_alloc(Leaf) {
    Ok(handle) => assert!(heap.get(handle).is_some()),
    Err(GcError::CapacityExhausted) => { /* run a collection and retry */ }
}
```

<hr>
<br>

<h2 id="usage-patterns">Usage Patterns</h2>

<h3 id="pattern-roots">Choosing roots</h3>

A root is any handle your runtime considers live from outside the heap: an interpreter's
value stack and locals, its global environment, VM registers, a work queue of pending
values. Everything reachable from a root survives; everything else is reclaimed. When in
doubt, over-root: passing an extra handle keeps its object alive one more cycle, while
under-rooting collects something still in use (whose handle then reads as `None`).

<h3 id="pattern-when">When to collect</h3>

`gc-lang` never collects on its own — you decide when. Common triggers: after every N
allocations, when [`len`](#heap-len) crosses a threshold, at the top of an evaluation
loop, or at explicit safe points where the root set is easy to enumerate. Collection
cost is linear in the reachable set plus the slot count, so collecting a mostly-garbage
heap is cheap; collecting a mostly-live one costs about `~12 ns` per object.

<h3 id="pattern-multiple-heaps">Multiple heaps</h3>

A program may run several heaps. The `T` tag on [`Gc<T>`](#gc) keeps their handles from
being confused at compile time when the heaps hold different types — but not when two
heaps hold the *same* type: a handle carries no heap identity, so keep track of which
heap each handle came from. Each heap collects independently against its own roots. An
object in one heap that holds a handle into another does not keep that object alive
through tracing ([`Tracer::mark`](#tracer)); pass such handles to the other heap's
`collect` as roots.

<h3 id="pattern-no-std">`no_std`</h3>

Disable default features to build without `std`:

```toml
[dependencies]
gc-lang = { version = "1.0", default-features = false }
```

The public surface is identical; the crate needs only `alloc` and a global allocator.

<hr>
<br>

<h2 id="stability">Stability &amp; SemVer Promise</h2>

**1.0.0 freezes the public surface.** The six exported items — [`Heap`](#heap),
[`Gc`](#gc), [`Trace`](#trace), [`Tracer`](#tracer), [`CollectStats`](#collectstats),
and [`GcError`](#gcerror) — and every public method, signature, and documented
behaviour listed on this page are stable. They will not change in a breaking way for
the entire `1.x` series.

Within `1.x`, the project guarantees:

- **No breaking changes.** No exported item is removed or renamed; no method signature,
  bound, or documented behaviour changes incompatibly. Anything that would break a
  downstream crate is a `2.0`.
- **Additive minor releases.** New methods, new types, and new trait impls may arrive in
  a `1.y` release. The two `#[non_exhaustive]` types — [`CollectStats`](#collectstats)
  and [`GcError`](#gcerror) — may gain fields or variants; they are already marked so a
  downstream `match` accounts for it.
- **Patch releases** carry bug fixes, documentation, and internal or performance work
  with no surface change.

Not covered by the promise: the exact wording of `Debug` output, precise benchmark
figures, internal slot layout, and the specific generation value a reused slot carries
(only the guarantee that a stale handle resolves to `None`). MSRV increases are treated
as at least a minor bump and recorded in the [CHANGELOG](../CHANGELOG.md).

<hr>
<br>

<h2 id="api-safety">API Safety</h2>

The whole crate is `#![forbid(unsafe_code)]`: there is no `unsafe` anywhere, so no
handle can produce undefined behaviour. The worst a misused handle can do is resolve to
`None`.

Read-and-return methods are annotated `#[must_use]`, so the compiler warns when their
result is dropped — ignoring it usually signals a logic error:

- `Heap`: `new`, `with_capacity`, `get`, `contains`, `len`, `is_empty`, `capacity`
- `try_alloc` returns `Result`, which already carries `#[must_use]`

`collect` is intentionally **not** `#[must_use]`: calling it purely for its reclamation
effect and ignoring the returned [`CollectStats`](#collectstats) is legitimate.

<hr>
<br>

<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>API</span>
        <span>&nbsp;│&nbsp;</span>
        <a href="../CHANGELOG.md" title="Changelog"><b>CHANGELOG</b></a>
    </sup>
</div>

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

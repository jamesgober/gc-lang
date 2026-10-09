//! The garbage-collected heap: allocation, resolution, and mark-and-sweep collection.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use crate::{Gc, GcError, Trace, Tracer};

/// One storage slot. An occupied slot holds its value; a free slot holds `None` and
/// waits on the free list to be handed out again. The generation advances each time
/// the slot is reclaimed, which is what invalidates handles into a slot's earlier
/// occupants.
///
/// A slot has exactly `2^32` generations, `0..=u32::MAX`, and so carries at most
/// `2^32` occupants over the heap's lifetime. When the occupant at generation
/// `u32::MAX` is reclaimed the slot is *retired*: it keeps `value: None` and
/// `generation: u32::MAX` forever and is never put back on the free list. Advancing
/// the counter by wrapping instead would hand generation `0` out again, and with a
/// LIFO free list a hot slot cycles through all `2^32` generations in minutes, after
/// which a stale handle would name the slot's new occupant (ABA). A retired slot can
/// never be occupied again, so no handle into it can ever resolve.
struct Slot<T> {
    value: Option<T>,
    generation: u32,
}

/// A garbage-collected heap of `T` objects, reclaimed by tracing mark-and-sweep.
///
/// A `Heap<T>` is the object store for an interpreted-language runtime: allocate a
/// value with [`alloc`](Heap::alloc) and hold the returned [`Gc<T>`] handle, wire
/// objects to each other by storing handles inside them, and periodically call
/// [`collect`](Heap::collect) with the runtime's roots to reclaim everything no
/// longer reachable. Cycles are collected — reachability, not reference counting,
/// decides what lives — so a runtime can build arbitrary object graphs without
/// leaking them.
///
/// The design is deliberately narrow and entirely safe (`#![forbid(unsafe_code)]`):
///
/// - **Handles, not pointers.** A [`Gc<T>`] is an index plus a generation stamp, so
///   objects can point at one another freely without borrows, and a handle to a
///   collected object resolves to `None` instead of dangling.
/// - **Slots are reused.** Sweeping a dead object bumps its slot's generation and
///   returns the slot to a free list; the next allocation reuses it. A slot serves
///   `2^32` occupants (generations `0..=u32::MAX`) and is then retired for good
///   rather than wrapping its generation back to `0`, so a stale handle stays stale
///   for the heap's whole lifetime. A steady-state allocate/collect loop therefore
///   grows the backing store by one slot each time one of its slots retires — once
///   per `2^32` reuses of that slot — and not otherwise.
/// - **Scratch is pooled.** The mark queue and the mark bitset are retained between
///   collections, so a collection allocates nothing on the steady-state path.
///
/// `T` must implement [`Trace`] to be collected, so the collector can follow the
/// handles each object owns. Resolution ([`get`](Heap::get)) and allocation do not
/// require `Trace`; only [`collect`](Heap::collect) does.
///
/// # Examples
///
/// Allocate a small graph, drop a root, and collect the unreachable part:
///
/// ```
/// use gc_lang::{Gc, Heap, Trace, Tracer};
///
/// struct Node {
///     edges: Vec<Gc<Node>>,
/// }
///
/// impl Trace for Node {
///     fn trace(&self, tracer: &mut Tracer<'_>) {
///         for &e in &self.edges {
///             tracer.mark(e);
///         }
///     }
/// }
///
/// let mut heap = Heap::new();
/// let leaf = heap.alloc(Node { edges: vec![] });
/// let root = heap.alloc(Node { edges: vec![leaf] });
/// let orphan = heap.alloc(Node { edges: vec![] });
///
/// assert_eq!(heap.len(), 3);
///
/// // Collect with `root` as the only root: `root` and `leaf` survive, `orphan` does not.
/// let stats = heap.collect([root]);
/// assert_eq!(stats.freed, 1);
/// assert_eq!(heap.len(), 2);
/// assert!(heap.get(orphan).is_none());
/// assert!(heap.get(leaf).is_some());
/// ```
pub struct Heap<T> {
    /// Object storage, indexed by slot. Grows on demand; never shrinks.
    slots: Vec<Slot<T>>,
    /// Indices of reclaimed slots available for reuse, most-recently-freed first.
    /// Never holds a retired slot (see [`Slot`]).
    free: Vec<u32>,
    /// Number of occupied slots. Tracked incrementally so [`len`](Heap::len) is O(1).
    live: usize,
    /// Retained mark-phase work queue: `(index, generation)` pairs. Pooled so a
    /// collection allocates nothing once the queue has grown to its working size.
    worklist: Vec<(u32, u32)>,
    /// Retained mark bitset, one bit per slot, packed 64 to a word. Pooled and
    /// cleared at the start of each collection.
    marks: Vec<u64>,
}

impl<T> Heap<T> {
    /// Creates an empty heap. `const`, so it can initialise a `static`.
    ///
    /// No allocation happens until the first value is added.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::Heap;
    ///
    /// let heap: Heap<u32> = Heap::new();
    /// assert!(heap.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            worklist: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Creates an empty heap with room for `capacity` objects preallocated.
    ///
    /// A hint only: it reserves backing storage so the first `capacity` allocations
    /// do not reallocate. Sizing it to the runtime's expected live-object count
    /// keeps allocation off the reallocation path during warm-up.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::Heap;
    ///
    /// let heap: Heap<u64> = Heap::with_capacity(1024);
    /// assert!(heap.capacity() >= 1024);
    /// ```
    #[inline]
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free: Vec::new(),
            live: 0,
            worklist: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Allocates `value` and returns a stable [`Gc<T>`] handle to it.
    ///
    /// This is the hot path. It reuses a slot freed by an earlier collection when
    /// one is available, and only grows the backing store otherwise. The handle
    /// stays valid until the object it names is collected.
    ///
    /// # Panics
    ///
    /// Panics only if the heap has exhausted its slot space: no slot is free and all
    /// `u32::MAX + 1` slot indices are either occupied or retired. A slot retires
    /// only after serving `2^32` occupants, so for a heap that collects this takes
    /// on the order of `2^64` allocations. Use [`try_alloc`](Heap::try_alloc) for an
    /// explicit non-panicking path.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Leaf;
    /// impl Trace for Leaf {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let handle = heap.alloc(Leaf);
    /// assert!(heap.get(handle).is_some());
    /// ```
    #[inline]
    pub fn alloc(&mut self, value: T) -> Gc<T> {
        match self.try_alloc(value) {
            Ok(handle) => handle,
            Err(_) => panic!("heap is full: cannot address beyond u32::MAX slots"),
        }
    }

    /// Allocates `value`, returning its [`Gc<T>`] handle or an error if the heap's
    /// slot space is exhausted.
    ///
    /// The non-panicking counterpart to [`alloc`](Heap::alloc): identical on
    /// success, but it returns [`GcError::CapacityExhausted`] instead of panicking
    /// at the slot-space ceiling. Prefer it when a runtime allocates in response to
    /// untrusted input whose volume it does not control.
    ///
    /// # Errors
    ///
    /// Returns [`GcError::CapacityExhausted`] when no slot is free and every one of
    /// the `u32::MAX + 1` slot indices is either occupied or retired (a slot
    /// retires after serving `2^32` occupants; see [`Heap`]). The heap is left
    /// unchanged. Running a collection to reclaim dead slots is the way to recover;
    /// if every slot is retired nothing can be reclaimed, and the heap cannot
    /// allocate again.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Leaf;
    /// impl Trace for Leaf {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let handle = heap.try_alloc(Leaf)?;
    /// assert!(heap.get(handle).is_some());
    /// # Ok::<(), gc_lang::GcError>(())
    /// ```
    #[inline]
    pub fn try_alloc(&mut self, value: T) -> Result<Gc<T>, GcError> {
        if let Some(index) = self.free.pop() {
            // Reuse a reclaimed slot. Its generation was advanced when it was freed,
            // so any handle to the slot's previous occupant no longer matches.
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            let generation = slot.generation;
            self.live += 1;
            return Ok(Gc::new(index, generation));
        }

        // No free slot: append a fresh one. The next index is the current slot
        // count; if that no longer fits in a `u32`, the slot space is exhausted.
        // Checked before the push, so a rejected allocation leaves the heap intact.
        let index = u32::try_from(self.slots.len()).map_err(|_| GcError::CapacityExhausted)?;
        self.slots.push(Slot {
            value: Some(value),
            generation: 0,
        });
        self.live += 1;
        Ok(Gc::new(index, 0))
    }

    /// Borrows the object behind `handle`, or `None` if the handle does not name a
    /// live object in this heap.
    ///
    /// Resolution is a direct slot lookup, not a search. The `None` case covers both
    /// an out-of-range handle and a stale one — a handle whose object was collected
    /// and whose slot has since moved on to a new generation — so resolving a
    /// handle this heap minted never reads an unrelated value. That holds for the
    /// heap's whole lifetime: generations never wrap (a slot whose counter is used
    /// up is retired instead), so a stale handle never becomes valid again.
    ///
    /// A [`Gc<T>`] does not record which heap minted it. A handle from a
    /// *different* `Heap<T>` of the same `T` is not detected: if its index and
    /// generation happen to match a live object here, it resolves to that object.
    /// Use each handle only with the heap that returned it.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Payload(u32);
    /// impl Trace for Payload {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let handle = heap.alloc(Payload(7));
    /// assert_eq!(heap.get(handle).map(|p| p.0), Some(7));
    /// ```
    #[inline]
    #[must_use]
    pub fn get(&self, handle: Gc<T>) -> Option<&T> {
        let slot = self.slots.get(handle.index() as usize)?;
        if slot.generation == handle.generation() {
            slot.value.as_ref()
        } else {
            None
        }
    }

    /// Mutably borrows the object behind `handle`, or `None` if the handle does not
    /// name a live object in this heap.
    ///
    /// The mutating counterpart to [`get`](Heap::get), with the same staleness
    /// guarantees. Use it to update an object in place — including rewiring the
    /// handles it holds, which is how a runtime mutates its object graph.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Cell(i64);
    /// impl Trace for Cell {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let handle = heap.alloc(Cell(0));
    /// if let Some(cell) = heap.get_mut(handle) {
    ///     cell.0 = 42;
    /// }
    /// assert_eq!(heap.get(handle).map(|c| c.0), Some(42));
    /// ```
    #[inline]
    pub fn get_mut(&mut self, handle: Gc<T>) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation == handle.generation() {
            slot.value.as_mut()
        } else {
            None
        }
    }

    /// Returns `true` if `handle` names a live object in this heap.
    ///
    /// Equivalent to `heap.get(handle).is_some()`, without producing a borrow.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Leaf;
    /// impl Trace for Leaf {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let handle = heap.alloc(Leaf);
    /// assert!(heap.contains(handle));
    /// ```
    #[inline]
    #[must_use]
    pub fn contains(&self, handle: Gc<T>) -> bool {
        match self.slots.get(handle.index() as usize) {
            Some(slot) => slot.generation == handle.generation() && slot.value.is_some(),
            None => false,
        }
    }

    /// Returns the number of live objects in the heap.
    ///
    /// This is the occupied-slot count, not the backing-store size; freed slots
    /// awaiting reuse are not counted.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Leaf;
    /// impl Trace for Leaf {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// assert_eq!(heap.len(), 0);
    /// heap.alloc(Leaf);
    /// assert_eq!(heap.len(), 1);
    /// ```
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Returns `true` if the heap holds no live objects.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Heap, Trace, Tracer};
    ///
    /// struct Leaf;
    /// impl Trace for Leaf {
    ///     fn trace(&self, _: &mut Tracer<'_>) {}
    /// }
    ///
    /// let mut heap = Heap::new();
    /// assert!(heap.is_empty());
    /// heap.alloc(Leaf);
    /// assert!(!heap.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Returns the number of slots the backing store can hold before it must grow.
    ///
    /// Reflects allocated capacity, including slots currently free and slots that
    /// have retired. It never decreases across a collection: sweeping returns slots
    /// to the free list rather than releasing memory, so the store stays sized to
    /// the high-water mark. A retired slot (one that has served `2^32` occupants;
    /// see [`Heap`]) keeps its place in the store but is never reused.
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::Heap;
    ///
    /// let heap: Heap<u64> = Heap::with_capacity(64);
    /// assert!(heap.capacity() >= 64);
    /// ```
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Reclaims every object not reachable from `roots`, returning what the pass did.
    ///
    /// This is a tracing mark-and-sweep collection in two phases. **Mark:** starting
    /// from each root, the collector follows the handles every object reports through
    /// [`Trace::trace`], visiting each reachable object exactly once — so cycles
    /// terminate and shared subgraphs are not re-scanned. **Sweep:** every object
    /// that was not marked is dropped, its slot's generation is advanced, and the
    /// slot is returned to the free list for reuse (or retired, if that was its last
    /// generation).
    ///
    /// `roots` is the set of handles the runtime considers live from the outside: an
    /// interpreter's value stack, its global environment, VM registers. Anything
    /// reachable from a root survives; anything else — including whole unreachable
    /// cycles — is reclaimed. A root handle that is already stale is ignored, so it
    /// is safe to pass a conservative, slightly-oversized root set.
    ///
    /// The cost is `O(reachable)` to mark plus `O(slots)` to sweep. The mark queue
    /// and mark bitset are retained between calls, so a steady-state collection does
    /// not allocate.
    ///
    /// # Panics
    ///
    /// `collect` itself does not panic, but it runs user code — [`Trace::trace`]
    /// during mark and `T`'s destructor during sweep — and a panic there propagates
    /// out of `collect`. The heap stays consistent when that happens: every slot
    /// swept before the panic, including the one whose destructor panicked, is
    /// already reclaimed (generation advanced, slot on the free list, [`len`] down
    /// by one), and every slot not yet swept still holds its object and counts as
    /// live. Unreachable objects the interrupted pass did not reach are reclaimed by
    /// the next collection. No slot is leaked, and [`len`] always equals the number
    /// of objects the heap still stores.
    ///
    /// [`len`]: Heap::len
    ///
    /// # Examples
    ///
    /// Two objects in a cycle, unreachable from any root, are still collected:
    ///
    /// ```
    /// use gc_lang::{Gc, Heap, Trace, Tracer};
    ///
    /// struct Node {
    ///     link: Option<Gc<Node>>,
    /// }
    /// impl Trace for Node {
    ///     fn trace(&self, tracer: &mut Tracer<'_>) {
    ///         if let Some(link) = self.link {
    ///             tracer.mark(link);
    ///         }
    ///     }
    /// }
    ///
    /// let mut heap = Heap::new();
    /// let a = heap.alloc(Node { link: None });
    /// let b = heap.alloc(Node { link: Some(a) });
    /// heap.get_mut(a).unwrap().link = Some(b); // a <-> b cycle, no external root
    ///
    /// let stats = heap.collect([]); // empty root set
    /// assert_eq!(stats.freed, 2);
    /// assert!(heap.is_empty());
    /// ```
    pub fn collect<I>(&mut self, roots: I) -> CollectStats
    where
        I: IntoIterator<Item = Gc<T>>,
        T: Trace,
    {
        // Split the borrow of `self` by field: the mark loop reads `slots` while
        // `Trace::trace` pushes into `worklist`. Working on the pooled fields in
        // place (rather than moving the queue out and back) also means a panic in
        // user code cannot strand the pooled allocations.
        let Self {
            slots,
            free,
            live,
            worklist,
            marks,
        } = self;

        // Seed the work queue with the roots. Reuse the pooled queue so a
        // steady-state collection does not allocate. Roots are validated at pop
        // time along with everything else, so stale roots cost nothing here.
        worklist.clear();
        for root in roots {
            worklist.push((root.index(), root.generation()));
        }

        // A fresh, zeroed mark bit for every slot. The allocation is reused across
        // collections; only a grow past the high-water mark reallocates.
        marks.clear();
        marks.resize(slots.len().div_ceil(64), 0);

        // Mark: drain the queue, marking each live, current, not-yet-marked slot and
        // enqueuing the handles it reports. The mark check makes cycles terminate.
        while let Some((index, generation)) = worklist.pop() {
            let i = index as usize;
            let value = match slots.get(i) {
                Some(slot) if slot.generation == generation => match slot.value.as_ref() {
                    Some(value) => value,
                    None => continue,
                },
                _ => continue,
            };
            if bit_is_set(marks, i) {
                continue;
            }
            set_bit(marks, i);
            value.trace(&mut Tracer::new(worklist));
        }

        // Sweep: reclaim every unmarked occupant. All of a slot's bookkeeping —
        // taking the object out, advancing or retiring the generation, the free
        // list, the live count — is finished *before* the object's destructor runs.
        // That ordering is the drop guard: if `T::drop` panics, the unwind leaves
        // through a heap whose every slot is either fully reclaimed or untouched,
        // so nothing leaks and `live` stays exact. The slots after the panicking
        // one are simply not swept this time; they still hold their objects and
        // still count as live, and the next collection reclaims them.
        let mut freed = 0usize;
        for (i, slot) in slots.iter_mut().enumerate() {
            if bit_is_set(marks, i) {
                continue;
            }
            let Some(dead) = slot.value.take() else {
                continue;
            };
            // Advance the generation so every handle to `dead` goes stale, and offer
            // the slot for reuse. `i` fits in a `u32`: `try_alloc` never creates a
            // slot index above `u32::MAX`. When there is no next generation the
            // slot has served its last occupant and is retired instead: it stays
            // empty at `u32::MAX` and never returns to the free list, so no
            // generation is ever handed out twice (no ABA on wrap).
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
                free.push(i as u32);
            }
            *live -= 1;
            freed += 1;
            drop(dead);
        }

        CollectStats { live: *live, freed }
    }

    /// Moves a free slot's generation forward to `generation`, so tests can reach
    /// the end of a slot's generation range without `2^32` real reuses.
    ///
    /// Test-only, and only ever forward: moving a generation backwards could
    /// re-validate a stale handle, which is the exact bug this hook exists to test.
    #[cfg(test)]
    pub(crate) fn advance_free_slot_generation(&mut self, index: u32, generation: u32) {
        let slot = &mut self.slots[index as usize];
        assert!(slot.value.is_none(), "slot {index} is occupied");
        assert!(self.free.contains(&index), "slot {index} is not free");
        assert!(
            generation >= slot.generation,
            "generations only move forward"
        );
        slot.generation = generation;
    }
}

/// Reads the mark bit for slot `i`.
#[inline]
fn bit_is_set(marks: &[u64], i: usize) -> bool {
    (marks[i >> 6] >> (i & 63)) & 1 == 1
}

/// Sets the mark bit for slot `i`.
#[inline]
fn set_bit(marks: &mut [u64], i: usize) {
    marks[i >> 6] |= 1u64 << (i & 63);
}

impl<T> Default for Heap<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Heap<T> {
    /// Shows the heap's shape — live objects, free slots, capacity — not its
    /// contents. A heap can hold millions of objects, and dumping them is rarely
    /// what a debug print wants; this also keeps `Heap<T>: Debug` free of a
    /// `T: Debug` bound.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Heap")
            .field("live", &self.live)
            .field("free", &self.free.len())
            .field("capacity", &self.slots.capacity())
            .finish()
    }
}

/// What a [`collect`](Heap::collect) pass did.
///
/// Returned by [`Heap::collect`]. `live + freed` equals the number of objects that
/// were resident when the pass began.
///
/// The struct is `#[non_exhaustive]`: a later phase may report more (bytes
/// reclaimed, pause time), so construct it only through the collector and read the
/// fields you need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectStats {
    /// Objects that survived the collection — the reachable set.
    pub live: usize,
    /// Objects reclaimed by the collection — the unreachable set.
    pub freed: usize,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// A node with arbitrary outgoing edges — enough to build any object graph.
    struct Node {
        edges: Vec<Gc<Node>>,
    }

    impl Node {
        fn leaf() -> Self {
            Self { edges: Vec::new() }
        }
        fn with(edges: Vec<Gc<Node>>) -> Self {
            Self { edges }
        }
    }

    impl Trace for Node {
        fn trace(&self, tracer: &mut Tracer<'_>) {
            for &edge in &self.edges {
                tracer.mark(edge);
            }
        }
    }

    #[test]
    fn test_alloc_then_get_round_trips() {
        let mut heap = Heap::new();
        let handle = heap.alloc(Node::leaf());
        assert!(heap.get(handle).is_some());
        assert!(heap.contains(handle));
        assert_eq!(heap.len(), 1);
        assert!(!heap.is_empty());
    }

    #[test]
    fn test_unreachable_object_is_collected() {
        let mut heap = Heap::new();
        let keep = heap.alloc(Node::leaf());
        let drop = heap.alloc(Node::leaf());
        let stats = heap.collect([keep]);
        assert_eq!(stats.freed, 1);
        assert_eq!(stats.live, 1);
        assert!(heap.get(keep).is_some());
        assert!(heap.get(drop).is_none());
    }

    #[test]
    fn test_reachable_subgraph_survives() {
        let mut heap = Heap::new();
        let leaf = heap.alloc(Node::leaf());
        let mid = heap.alloc(Node::with(vec![leaf]));
        let root = heap.alloc(Node::with(vec![mid]));
        let orphan = heap.alloc(Node::leaf());

        let stats = heap.collect([root]);
        assert_eq!(stats.freed, 1); // only the orphan
        assert!(heap.get(root).is_some());
        assert!(heap.get(mid).is_some());
        assert!(heap.get(leaf).is_some());
        assert!(heap.get(orphan).is_none());
    }

    #[test]
    fn test_cycle_with_no_root_is_collected() {
        let mut heap = Heap::new();
        let a = heap.alloc(Node::leaf());
        let b = heap.alloc(Node::with(vec![a]));
        heap.get_mut(a).expect("a is live").edges.push(b); // a <-> b
        let stats = heap.collect([]);
        assert_eq!(stats.freed, 2);
        assert!(heap.is_empty());
    }

    #[test]
    fn test_self_cycle_is_collected() {
        let mut heap = Heap::new();
        let a = heap.alloc(Node::leaf());
        heap.get_mut(a).expect("a is live").edges.push(a); // a -> a
        let stats = heap.collect([]);
        assert_eq!(stats.freed, 1);
        assert!(heap.get(a).is_none());
    }

    #[test]
    fn test_freed_slot_is_reused_and_old_handle_goes_stale() {
        let mut heap = Heap::new();
        let first = heap.alloc(Node::leaf());
        let _ = heap.collect([]); // frees `first`'s slot
        assert!(heap.get(first).is_none());

        // The next allocation reuses the slot but at a new generation.
        let second = heap.alloc(Node::leaf());
        assert_eq!(first.index(), second.index());
        assert_ne!(first.generation(), second.generation());
        assert!(heap.get(second).is_some());
        assert!(heap.get(first).is_none()); // the stale handle stays dead
    }

    #[test]
    fn test_capacity_does_not_grow_across_steady_state_loop() {
        let mut heap: Heap<Node> = Heap::with_capacity(4);
        let baseline = heap.capacity();
        for _ in 0..1000 {
            let a = heap.alloc(Node::leaf());
            let b = heap.alloc(Node::leaf());
            let _ = heap.alloc(Node::with(vec![a, b]));
            let _ = heap.collect([]); // nothing rooted: reclaim all three
        }
        assert!(heap.is_empty());
        assert_eq!(
            heap.capacity(),
            baseline,
            "slots should be reused, not grown"
        );
    }

    #[test]
    fn test_collect_twice_is_idempotent_on_survivors() {
        let mut heap = Heap::new();
        let root = heap.alloc(Node::leaf());
        let s1 = heap.collect([root]);
        let s2 = heap.collect([root]);
        assert_eq!(s1.live, 1);
        assert_eq!(s2.freed, 0);
        assert_eq!(s2.live, 1);
        assert!(heap.get(root).is_some());
    }

    #[test]
    fn test_stale_root_is_ignored() {
        let mut heap = Heap::new();
        let gone = heap.alloc(Node::leaf());
        let _ = heap.collect([]); // `gone` is now stale
        let live = heap.alloc(Node::leaf());
        // Passing the stale handle as a root must not resurrect anything or panic.
        let stats = heap.collect([gone, live]);
        assert_eq!(stats.live, 1);
        assert!(heap.get(live).is_some());
    }

    #[test]
    fn test_out_of_range_handle_resolves_to_none() {
        let mut big = Heap::new();
        let mut last = big.alloc(Node::leaf());
        for _ in 0..50 {
            last = big.alloc(Node::leaf());
        }
        let small: Heap<Node> = Heap::new();
        assert!(small.get(last).is_none());
        assert!(!small.contains(last));
    }

    #[test]
    fn test_empty_heap_collect_is_a_noop() {
        let mut heap: Heap<Node> = Heap::new();
        let stats = heap.collect([]);
        assert_eq!(stats.freed, 0);
        assert_eq!(stats.live, 0);
    }

    #[test]
    fn test_debug_reports_shape_not_contents() {
        let mut heap = Heap::new();
        let _ = heap.alloc(Node::leaf());
        let text = alloc::format!("{heap:?}");
        assert!(text.contains("live"), "{text}");
        assert!(text.contains("capacity"), "{text}");
    }

    // --- M60: generations never wrap; a used-up slot is retired ------------------

    /// Frees slot 0 and moves its generation to `generation`, so the next
    /// allocation lands in slot 0 at that generation.
    fn heap_with_slot0_at(generation: u32) -> (Heap<Node>, Gc<Node>) {
        let mut heap = Heap::new();
        let first = heap.alloc(Node::leaf());
        let _ = heap.collect([]);
        heap.advance_free_slot_generation(0, generation);
        (heap, first)
    }

    #[test]
    fn test_slot_retires_at_last_generation_instead_of_wrapping() {
        let (mut heap, first) = heap_with_slot0_at(u32::MAX - 1);

        let penultimate = heap.alloc(Node::leaf());
        assert_eq!(
            (penultimate.index(), penultimate.generation()),
            (0, u32::MAX - 1)
        );
        let _ = heap.collect([]);

        let last = heap.alloc(Node::leaf());
        assert_eq!((last.index(), last.generation()), (0, u32::MAX));
        let stats = heap.collect([]);
        assert_eq!(stats.freed, 1);

        // Retired: empty, parked at the last generation, and not offered for reuse.
        assert!(heap.slots[0].value.is_none());
        assert_eq!(heap.slots[0].generation, u32::MAX);
        assert!(heap.free.is_empty());
        assert!(heap.is_empty());

        // The next allocation takes a fresh slot rather than wrapping slot 0 to 0.
        let fresh = heap.alloc(Node::leaf());
        assert_eq!((fresh.index(), fresh.generation()), (1, 0));
        for stale in [first, penultimate, last] {
            assert!(heap.get(stale).is_none());
            assert!(!heap.contains(stale));
        }
    }

    #[test]
    fn test_retired_slot_rejects_every_generation() {
        let (mut heap, _) = heap_with_slot0_at(u32::MAX);
        let _ = heap.alloc(Node::leaf());
        let _ = heap.collect([]); // slot 0 retires
        let keep = heap.alloc(Node::leaf()); // slot 1

        for generation in [0, 1, 2, u32::MAX / 2, u32::MAX - 1, u32::MAX] {
            let probe: Gc<Node> = Gc::new(0, generation);
            assert!(heap.get(probe).is_none(), "generation {generation}");
            assert!(heap.get_mut(probe).is_none(), "generation {generation}");
            assert!(!heap.contains(probe), "generation {generation}");
            // As a root it is ignored and resurrects nothing.
            let stats = heap.collect([probe, keep]);
            assert_eq!(stats.live, 1);
            assert_eq!(heap.len(), 1);
        }
        assert_invariants(&heap);
    }

    #[test]
    fn test_hot_slot_loop_across_retirement_never_revalidates_a_handle() {
        // The M60 scenario: an alloc/collect loop on a LIFO free list hammers one
        // slot. Start it a few generations from the end and run well past it.
        let (mut heap, first) = heap_with_slot0_at(u32::MAX - 5);
        let mut minted = vec![first];
        for _ in 0..32 {
            let h = heap.alloc(Node::leaf());
            assert!(!minted.contains(&h), "handle {h:?} was issued twice");
            minted.push(h);
            let _ = heap.collect([]);
            assert!(minted.iter().all(|&old| heap.get(old).is_none()));
            assert_invariants(&heap);
        }
        // Slot 0 served generations MAX-5..=MAX (six occupants) and retired; the
        // loop then moved to slot 1 and stayed there.
        assert_eq!(heap.slots.len(), 2);
        assert_eq!(heap.slots[0].generation, u32::MAX);
        assert_eq!(heap.free, vec![1]);
    }

    #[test]
    fn test_store_grows_by_exactly_one_slot_per_retirement() {
        // Three hot slots, all near the end of their range: each retires once, and
        // each retirement costs exactly one fresh slot, no more.
        let mut heap: Heap<Node> = Heap::new();
        for _ in 0..3 {
            let _ = heap.alloc(Node::leaf());
        }
        let _ = heap.collect([]);
        for i in 0..3 {
            heap.advance_free_slot_generation(i, u32::MAX);
        }
        for _ in 0..10 {
            for _ in 0..3 {
                let _ = heap.alloc(Node::leaf());
            }
            let _ = heap.collect([]);
        }
        assert_eq!(heap.slots.len(), 6);
        assert!(heap.slots[..3].iter().all(|s| s.generation == u32::MAX));
        assert_eq!(heap.free.len(), 3);
        assert_invariants(&heap);
    }

    #[test]
    fn test_free_slot_at_last_generation_is_still_allocatable() {
        // A free slot at `u32::MAX` has one occupant left in it; only reclaiming
        // that occupant retires it.
        let (mut heap, _) = heap_with_slot0_at(u32::MAX);
        let last = heap.alloc(Node::leaf());
        assert_eq!((last.index(), last.generation()), (0, u32::MAX));
        assert!(heap.get(last).is_some());
        let stats = heap.collect([last]);
        assert_eq!(stats.live, 1); // a rooted last-generation occupant survives
        assert!(heap.get(last).is_some());
    }
}

/// Structural invariants of a heap, checked by the unit and property tests:
/// `live` is exact, the free list holds only empty slots and holds each at most
/// once, and every empty slot not on the free list is retired (generation
/// `u32::MAX`). The last one is what "no slot leaks" means.
#[cfg(test)]
fn assert_invariants<T>(heap: &Heap<T>) {
    let occupied = heap.slots.iter().filter(|s| s.value.is_some()).count();
    assert_eq!(heap.live, occupied, "live count drifted");

    let mut on_free = alloc::vec![false; heap.slots.len()];
    for &i in &heap.free {
        let i = i as usize;
        assert!(!on_free[i], "slot {i} is on the free list twice");
        on_free[i] = true;
        assert!(heap.slots[i].value.is_none(), "free slot {i} is occupied");
    }
    for (i, slot) in heap.slots.iter().enumerate() {
        if slot.value.is_none() && !on_free[i] {
            assert_eq!(slot.generation, u32::MAX, "slot {i} leaked");
        }
    }
}

/// M61: a panicking destructor or `trace` must leave the heap consistent.
/// `catch_unwind` needs `std`.
#[cfg(all(test, feature = "std"))]
mod panic_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    /// An object whose destructor panics when armed.
    struct Bomb {
        armed: bool,
    }

    impl Drop for Bomb {
        fn drop(&mut self) {
            if self.armed {
                panic!("intentional test panic: Bomb::drop");
            }
        }
    }

    impl Trace for Bomb {
        fn trace(&self, _: &mut Tracer<'_>) {}
    }

    #[test]
    fn test_panicking_drop_neither_leaks_the_slot_nor_skews_live() {
        let mut heap = Heap::new();
        let a = heap.alloc(Bomb { armed: false });
        let b = heap.alloc(Bomb { armed: true });
        let c = heap.alloc(Bomb { armed: false });
        let d = heap.alloc(Bomb { armed: false });

        let result = catch_unwind(AssertUnwindSafe(|| heap.collect([d])));
        assert!(result.is_err(), "the destructor's panic propagates");

        // `a` and `b` were swept before the panic; the panicking slot is fully
        // reclaimed, not leaked. `c` was not reached and is still stored.
        assert!(heap.get(a).is_none());
        assert!(heap.get(b).is_none());
        assert!(heap.get(c).is_some());
        assert!(heap.get(d).is_some());
        assert_eq!(heap.len(), 2);
        assert_eq!(heap.free, vec![0, 1]);
        assert_eq!(heap.slots[1].generation, 1);
        assert_invariants(&heap);

        // The next collection finishes the job and the slots are reused.
        let stats = heap.collect([d]);
        assert_eq!((stats.live, stats.freed), (1, 1));
        assert!(heap.get(c).is_none());
        for _ in 0..3 {
            let _ = heap.alloc(Bomb { armed: false });
        }
        assert_eq!(heap.slots.len(), 4, "freed slots were reused, not leaked");
        assert_invariants(&heap);
    }

    #[test]
    fn test_panicking_drop_in_last_generation_still_retires_the_slot() {
        let mut heap = Heap::new();
        let _ = heap.alloc(Bomb { armed: false });
        let _ = heap.collect([]);
        heap.advance_free_slot_generation(0, u32::MAX);
        let bomb = heap.alloc(Bomb { armed: true });

        let result = catch_unwind(AssertUnwindSafe(|| heap.collect([])));
        assert!(result.is_err());

        assert!(heap.get(bomb).is_none());
        assert!(heap.is_empty());
        assert!(
            heap.free.is_empty(),
            "a retired slot is never offered again"
        );
        assert_eq!(heap.slots[0].generation, u32::MAX);
        assert_invariants(&heap);
        let next = heap.alloc(Bomb { armed: false });
        assert_eq!(next.index(), 1);
    }

    /// An object whose `trace` panics when armed.
    struct Tripwire {
        armed: bool,
    }

    impl Trace for Tripwire {
        fn trace(&self, _: &mut Tracer<'_>) {
            if self.armed {
                panic!("intentional test panic: Tripwire::trace");
            }
        }
    }

    #[test]
    fn test_panicking_trace_leaves_the_heap_untouched() {
        let mut heap = Heap::new();
        let wire = heap.alloc(Tripwire { armed: true });
        let garbage = heap.alloc(Tripwire { armed: false });

        let result = catch_unwind(AssertUnwindSafe(|| heap.collect([wire])));
        assert!(result.is_err());

        // A panic in the mark phase happens before anything is swept.
        assert_eq!(heap.len(), 2);
        assert!(heap.get(wire).is_some());
        assert!(heap.get(garbage).is_some());
        assert_invariants(&heap);

        heap.get_mut(wire).unwrap().armed = false;
        let stats = heap.collect([wire]);
        assert_eq!((stats.live, stats.freed), (1, 1));
        assert!(heap.get(garbage).is_none());
        assert_invariants(&heap);
    }
}

/// Property: across any interleaving of allocations, collections (with stale
/// handles mixed into the roots), and jumps of free slots to the end of their
/// generation range, no handle is ever issued twice and no stale handle ever
/// resolves — checked against a model that just remembers which handles are alive.
#[cfg(test)]
mod generation_props {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    extern crate alloc;
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;

    use proptest::prelude::*;

    use super::*;

    /// A leaf carrying a unique id, so resolving a handle to the *wrong* object is
    /// caught even if the generation check were fooled.
    struct Tagged(u64);

    impl Trace for Tagged {
        fn trace(&self, _: &mut Tracer<'_>) {}
    }

    #[derive(Debug, Clone)]
    enum Op {
        Alloc,
        /// Root the minted handles whose position matches a `true` flag (cycled);
        /// dead handles are included, so stale roots are exercised.
        Collect(Vec<bool>),
        /// Move a free slot to `u32::MAX - back` (never backwards).
        Jump {
            pick: usize,
            back: u32,
        },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            5 => Just(Op::Alloc),
            2 => prop::collection::vec(any::<bool>(), 0..48).prop_map(Op::Collect),
            1 => (any::<usize>(), 0u32..4).prop_map(|(pick, back)| Op::Jump { pick, back }),
        ]
    }

    proptest! {
        #[test]
        fn no_stale_handle_ever_validates(ops in prop::collection::vec(op(), 1..300)) {
            let mut heap: Heap<Tagged> = Heap::new();
            // (handle, id, alive) for every handle ever minted.
            let mut minted: Vec<(Gc<Tagged>, u64, bool)> = Vec::new();
            let mut issued: BTreeSet<Gc<Tagged>> = BTreeSet::new();

            for op in ops {
                match op {
                    Op::Alloc => {
                        let id = minted.len() as u64;
                        let h = heap.alloc(Tagged(id));
                        prop_assert!(issued.insert(h), "handle {:?} issued twice", h);
                        minted.push((h, id, true));
                    }
                    Op::Collect(flags) => {
                        let rooted = |k: usize| !flags.is_empty() && flags[k % flags.len()];
                        let roots: Vec<Gc<Tagged>> = minted
                            .iter()
                            .enumerate()
                            .filter(|&(k, _)| rooted(k))
                            .map(|(_, &(h, _, _))| h)
                            .collect();
                        let before = heap.len();
                        let stats = heap.collect(roots);
                        for (k, entry) in minted.iter_mut().enumerate() {
                            entry.2 = entry.2 && rooted(k);
                        }
                        prop_assert_eq!(stats.live + stats.freed, before);
                    }
                    Op::Jump { pick, back } => {
                        if !heap.free.is_empty() {
                            let index = heap.free[pick % heap.free.len()];
                            let current = heap.slots[index as usize].generation;
                            heap.advance_free_slot_generation(index, (u32::MAX - back).max(current));
                        }
                    }
                }

                let alive = minted.iter().filter(|e| e.2).count();
                prop_assert_eq!(heap.len(), alive);
                for &(h, id, is_alive) in &minted {
                    let expected = is_alive.then_some(id);
                    prop_assert_eq!(heap.get(h).map(|t| t.0), expected, "handle {:?}", h);
                    prop_assert_eq!(heap.contains(h), is_alive, "handle {:?}", h);
                }
                assert_invariants(&heap);
            }
        }
    }
}

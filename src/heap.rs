//! The garbage-collected heap: allocation, resolution, and mark-and-sweep collection.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use crate::{Gc, GcError, Trace, Tracer};

/// One storage slot. An occupied slot holds its value; a free slot holds `None` and
/// waits on the free list to be handed out again. The generation advances each time
/// the slot is reclaimed, which is what invalidates handles into a slot's earlier
/// occupants.
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
/// - **Slots are reused.** Sweeping a dead object returns its slot to a free list;
///   the next allocation reuses it and bumps its generation. Steady-state
///   allocate/collect loops do not grow the backing store.
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
    /// Panics only if the heap has already exhausted its slot space — more than
    /// `u32::MAX` slots that were never reclaimed, an unreachable ceiling for a
    /// heap that collects. Use [`try_alloc`](Heap::try_alloc) for an explicit
    /// non-panicking path.
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
    /// Returns [`GcError::CapacityExhausted`] when every one of the `u32::MAX + 1`
    /// slot indices is in use and no slot is free. The heap is left unchanged;
    /// running a collection to reclaim dead slots is the way to recover.
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
    /// handle never reads an unrelated value.
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
    /// Reflects allocated capacity, including slots currently free. It never
    /// decreases across a collection: sweeping returns slots to the free list rather
    /// than releasing memory, so the store stays sized to the high-water mark.
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
    /// slot is returned to the free list for reuse.
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
        // Seed the work queue with the roots. Reuse the pooled queue so a
        // steady-state collection does not allocate. Roots are validated at pop
        // time along with everything else, so stale roots cost nothing here.
        let mut work = core::mem::take(&mut self.worklist);
        work.clear();
        for root in roots {
            work.push((root.index(), root.generation()));
        }

        // A fresh, zeroed mark bit for every slot.
        self.reset_marks();

        // Mark: drain the queue, marking each live, current, not-yet-marked slot and
        // enqueuing the handles it reports. The mark check makes cycles terminate.
        while let Some((index, generation)) = work.pop() {
            let i = index as usize;
            let current = matches!(
                self.slots.get(i),
                Some(slot) if slot.generation == generation && slot.value.is_some()
            );
            if !current || bit_is_set(&self.marks, i) {
                continue;
            }
            set_bit(&mut self.marks, i);
            // Immutable borrow of `slots` plus a mutable borrow of the local `work`:
            // disjoint, so tracing children needs no unsafe and no second pass.
            if let Some(value) = self.slots[i].value.as_ref() {
                value.trace(&mut Tracer::new(&mut work));
            }
        }

        // Sweep: drop every unmarked occupant, advance its slot's generation to
        // invalidate outstanding handles, and return the slot to the free list.
        let mut freed = 0usize;
        for i in 0..self.slots.len() {
            let marked = bit_is_set(&self.marks, i);
            let slot = &mut self.slots[i];
            if slot.value.is_some() && !marked {
                slot.value = None; // drops the object
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(i as u32);
                freed += 1;
            }
        }
        self.live -= freed;

        // Return the pooled queue for the next collection to reuse.
        self.worklist = work;

        CollectStats {
            live: self.live,
            freed,
        }
    }

    /// Resizes the mark bitset to cover every slot and zeroes it. The allocation is
    /// reused across collections; only a grow past the high-water mark reallocates.
    #[inline]
    fn reset_marks(&mut self) {
        let words = self.slots.len().div_ceil(64);
        self.marks.clear();
        self.marks.resize(words, 0);
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
}

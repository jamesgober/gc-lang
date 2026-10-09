//! Pins the documented 1.x behaviour of handles used outside the heap that minted
//! them (ISSUES M61): `Gc<T>` carries no heap identity, and `Tracer::mark` accepts
//! a handle of any type and reads it as a position in the *collecting* heap.
//!
//! None of this is unsound — a misused handle can never dangle — but it is what the
//! rustdoc on `Gc` and `Tracer::mark` warns about. A 2.0 that types `mark` and
//! stamps handles with a heap id will make these tests fail on purpose.

use gc_lang::{Gc, Heap, Trace, Tracer};

struct Leaf(u32);

impl Trace for Leaf {
    fn trace(&self, _: &mut Tracer<'_>) {}
}

/// An object in one heap that holds a handle into another.
struct Holder {
    foreign: Gc<Leaf>,
}

impl Trace for Holder {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        // Compiles: `mark` is generic over the handle's type.
        tracer.mark(self.foreign);
    }
}

#[test]
fn a_same_type_handle_from_another_heap_is_not_detected() {
    let mut a = Heap::new();
    let mut b = Heap::new();
    let _in_a = a.alloc(Leaf(1));
    let in_b = b.alloc(Leaf(2));

    // Same index and generation in a different heap of the same type: `a` cannot
    // tell, and resolves it to its own, unrelated object.
    assert_eq!(a.get(in_b).map(|l| l.0), Some(1));
}

#[test]
fn marking_a_foreign_handle_does_not_keep_its_object_alive() {
    let mut leaves = Heap::new();
    let mut holders = Heap::new();
    let leaf = leaves.alloc(Leaf(7));
    let holder = holders.alloc(Holder { foreign: leaf });

    // The holder survives its own heap's collection and marks `leaf` while doing
    // so, but that mark lands in `holders`, not in `leaves`.
    let _ = holders.collect([holder]);
    assert!(holders.get(holder).is_some());

    // `leaves` only keeps what its own roots reach.
    let _ = leaves.collect([]);
    assert!(leaves.get(leaf).is_none());

    // The supported pattern: root cross-heap references explicitly.
    let leaf = leaves.alloc(Leaf(8));
    if let Some(h) = holders.get_mut(holder) {
        h.foreign = leaf;
    }
    let roots: Vec<Gc<Leaf>> = holders.get(holder).map(|h| h.foreign).into_iter().collect();
    let _ = leaves.collect(roots);
    assert!(leaves.get(leaf).is_some());
}

#[test]
fn marking_a_foreign_handle_can_retain_an_unrelated_object() {
    let mut leaves = Heap::new();
    let mut holders = Heap::new();

    // Slot 0 of `leaves` and slot 0 of `holders` both sit at generation 0, so the
    // foreign handle's (index, generation) coincides with `unrooted` in `holders`.
    let foreign = leaves.alloc(Leaf(0));
    let unrooted = holders.alloc(Holder { foreign });
    let root = holders.alloc(Holder { foreign });

    // Only `root` is a root, and nothing in `holders` refers to `unrooted`, yet it
    // survives: `root`'s mark of the foreign handle is read as slot 0 here.
    let stats = holders.collect([root]);
    assert_eq!(stats.freed, 0);
    assert!(holders.get(unrooted).is_some());
}

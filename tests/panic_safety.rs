//! A panicking destructor or `trace` during `collect` leaves the heap consistent
//! (ISSUES M61): no slot leaks, `len` stays exact, and the next collection finishes
//! the job. Checked through the public API only; `src/heap.rs` checks the internal
//! slot and free-list state as well.

use std::panic::{AssertUnwindSafe, catch_unwind};

use gc_lang::{Heap, Trace, Tracer};

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
fn panicking_drop_keeps_len_exact_and_slots_reusable() {
    let mut heap = Heap::with_capacity(8);
    let keep = heap.alloc(Bomb { armed: false });
    let before = heap.alloc(Bomb { armed: false });
    let bomb = heap.alloc(Bomb { armed: true });
    let after = heap.alloc(Bomb { armed: false });
    let capacity = heap.capacity();

    let result = catch_unwind(AssertUnwindSafe(|| heap.collect([keep])));
    assert!(
        result.is_err(),
        "the destructor's panic propagates out of collect"
    );

    // Swept before or at the panic: gone. Not yet reached: still stored.
    assert!(heap.get(before).is_none());
    assert!(heap.get(bomb).is_none());
    assert!(heap.get(after).is_some());
    assert!(heap.get(keep).is_some());
    assert_eq!(heap.len(), 2);

    // The next collection reclaims what the interrupted one did not reach.
    let stats = heap.collect([keep]);
    assert_eq!((stats.live, stats.freed), (1, 1));
    assert_eq!(heap.len(), 1);

    // Every reclaimed slot, including the one whose destructor panicked, is reused:
    // a long alloc/collect loop afterwards never grows the store.
    for _ in 0..1_000 {
        for _ in 0..3 {
            let _ = heap.alloc(Bomb { armed: false });
        }
        let _ = heap.collect([keep]);
    }
    assert_eq!(heap.len(), 1);
    assert_eq!(heap.capacity(), capacity, "no slot leaked");
}

#[test]
fn repeated_panicking_drops_never_skew_len() {
    let mut heap = Heap::new();
    let root = heap.alloc(Bomb { armed: false });
    for round in 0..50 {
        let _ = heap.alloc(Bomb { armed: false });
        let _ = heap.alloc(Bomb { armed: true });
        let _ = heap.alloc(Bomb { armed: false });
        // Each armed bomb panics exactly once; keep collecting until a pass
        // completes. `len` must equal the survivors at every step.
        while catch_unwind(AssertUnwindSafe(|| heap.collect([root]))).is_err() {}
        assert_eq!(heap.len(), 1, "round {round}");
    }
    assert!(heap.get(root).is_some());
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
fn panicking_trace_reclaims_nothing_and_recovers() {
    let mut heap = Heap::new();
    let wire = heap.alloc(Tripwire { armed: true });
    let garbage = heap.alloc(Tripwire { armed: false });

    let result = catch_unwind(AssertUnwindSafe(|| heap.collect([wire])));
    assert!(result.is_err());
    assert_eq!(heap.len(), 2, "a mark-phase panic sweeps nothing");

    if let Some(w) = heap.get_mut(wire) {
        w.armed = false;
    }
    let stats = heap.collect([wire]);
    assert_eq!((stats.live, stats.freed), (1, 1));
    assert!(heap.get(garbage).is_none());
}

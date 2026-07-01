//! Collecting a reference cycle.
//!
//! A cycle is the case reference counting cannot reclaim on its own: two objects
//! that point at each other keep each other's count above zero forever. A tracing
//! collector decides by reachability instead, so once nothing outside the cycle
//! refers to it, the whole cycle is freed.
//!
//! Run with: `cargo run --example cycle_collection`

use gc_lang::{Gc, Heap, Trace, Tracer};

/// A node in a doubly-linked ring: it points forward, and the ring points back.
struct Node {
    label: &'static str,
    next: Option<Gc<Node>>,
}

impl Trace for Node {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        if let Some(next) = self.next {
            tracer.mark(next);
        }
    }
}

fn main() {
    let mut heap = Heap::new();

    // Build a three-node ring: a -> b -> c -> a. Every node is reachable from every
    // other, so reference counts would never fall to zero.
    let a = heap.alloc(Node {
        label: "a",
        next: None,
    });
    let b = heap.alloc(Node {
        label: "b",
        next: Some(a),
    });
    let c = heap.alloc(Node {
        label: "c",
        next: Some(b),
    });
    heap.get_mut(a).expect("a is live").next = Some(c); // close the ring

    println!("built a 3-node ring, heap now holds {} objects", heap.len());

    // Keep the ring rooted at `a`: nothing is collected, because everything is
    // reachable from the root.
    let kept = heap.collect([a]);
    println!("collect([a]) -> freed {}, live {}", kept.freed, kept.live);
    assert_eq!(kept.freed, 0);

    // Walk the ring once to show it is intact.
    let mut cursor = a;
    print!("ring from a: {}", heap.get(a).expect("a is live").label);
    for _ in 0..3 {
        cursor = heap
            .get(cursor)
            .expect("node is live")
            .next
            .expect("ring is closed");
        print!(" -> {}", heap.get(cursor).expect("node is live").label);
    }
    println!();

    // Now drop the last external reference by collecting with an empty root set.
    // The ring is unreachable, so all three nodes go at once.
    let swept = heap.collect([]);
    println!("collect([]) -> freed {}, live {}", swept.freed, swept.live);
    assert_eq!(swept.freed, 3);
    assert!(heap.is_empty());
    assert!(heap.get(a).is_none()); // the old handle now reads as absent

    println!("the cycle was reclaimed; the heap is empty");
}

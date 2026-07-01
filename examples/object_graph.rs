//! Reachability over a shared object graph.
//!
//! Objects can be shared by several parents and still be collected correctly: a node
//! survives as long as *any* root reaches it, and is reclaimed the moment the last
//! path to it disappears. This example wires a small diamond-shaped graph, then
//! removes roots one at a time to watch the reachable frontier shrink.
//!
//! Run with: `cargo run --example object_graph`

use gc_lang::{Gc, Heap, Trace, Tracer};

struct Node {
    name: &'static str,
    children: Vec<Gc<Node>>,
}

impl Trace for Node {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        for &child in &self.children {
            tracer.mark(child);
        }
    }
}

fn main() {
    let mut heap = Heap::new();

    // A diamond plus a private leaf:
    //
    //        root_x        root_y
    //         /  \          /
    //        a    b        b
    //         \  /         |
    //         shared       c
    //
    // `shared` has two parents (a and b); `c` hangs only off `b`.
    let shared = heap.alloc(Node {
        name: "shared",
        children: vec![],
    });
    let c = heap.alloc(Node {
        name: "c",
        children: vec![],
    });
    let a = heap.alloc(Node {
        name: "a",
        children: vec![shared],
    });
    let b = heap.alloc(Node {
        name: "b",
        children: vec![shared, c],
    });
    let root_x = heap.alloc(Node {
        name: "root_x",
        children: vec![a, b],
    });
    let root_y = heap.alloc(Node {
        name: "root_y",
        children: vec![b],
    });

    let names = |heap: &Heap<Node>, handles: &[Gc<Node>]| {
        handles
            .iter()
            .filter_map(|&h| heap.get(h).map(|n| n.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let everything = [shared, c, a, b, root_x, root_y];

    println!("initial live objects: {}", heap.len());

    // Root both: the entire graph is reachable, nothing is collected.
    let stats = heap.collect([root_x, root_y]);
    println!(
        "roots {{root_x, root_y}} -> freed {}, live: [{}]",
        stats.freed,
        names(&heap, &everything)
    );
    assert_eq!(stats.freed, 0);

    // Drop root_x. `a` was only reachable through root_x, so it goes — but `shared`
    // is still reachable through b, so it stays.
    let stats = heap.collect([root_y]);
    println!(
        "roots {{root_y}}         -> freed {}, live: [{}]",
        stats.freed,
        names(&heap, &everything)
    );
    assert!(heap.get(a).is_none());
    assert!(heap.get(shared).is_some());
    assert!(heap.get(root_x).is_none());

    // Drop the last root: the whole remaining graph is unreachable.
    let stats = heap.collect([]);
    println!(
        "roots {{}}               -> freed {}, live: []",
        stats.freed
    );
    assert!(heap.is_empty());
}

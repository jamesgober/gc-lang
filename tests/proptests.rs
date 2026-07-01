//! Property-based tests for the collector's core invariant: after a collection, an
//! object is live if and only if it was reachable from a root.
//!
//! Each test builds an arbitrary object graph — arbitrary node count, arbitrary
//! edges including cycles and self-loops, an arbitrary root subset — then compares
//! the heap's post-collection state against an independent breadth-first reachability
//! computation. The reference walk is deliberately not the collector's algorithm, so
//! agreement between them is real evidence rather than a restatement.

use std::collections::{BTreeSet, VecDeque};

use gc_lang::{Gc, Heap, Trace, Tracer};
use proptest::prelude::*;

/// A node whose outgoing edges are handles into the same heap.
struct Node {
    edges: Vec<Gc<Node>>,
}

impl Trace for Node {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        for &edge in &self.edges {
            tracer.mark(edge);
        }
    }
}

/// A generated graph: `node_count` nodes, `edges[i]` listing the targets of node `i`
/// (as node indices, wrapped into range), and `roots` as a set of node indices.
#[derive(Debug, Clone)]
struct GraphSpec {
    node_count: usize,
    edges: Vec<Vec<usize>>,
    roots: BTreeSet<usize>,
}

fn graph_strategy() -> impl Strategy<Value = GraphSpec> {
    (1usize..40).prop_flat_map(|node_count| {
        // Each node gets 0..=6 edges; each edge is an arbitrary index mapped into
        // range so it always names a real node. Roots are an arbitrary index subset.
        let edges = prop::collection::vec(
            prop::collection::vec(any::<usize>().prop_map(move |t| t % node_count), 0..=6),
            node_count,
        );
        let roots = prop::collection::vec(any::<bool>(), node_count);
        (Just(node_count), edges, roots).prop_map(|(node_count, edges, root_flags)| {
            let roots = root_flags
                .into_iter()
                .enumerate()
                .filter_map(|(i, keep)| keep.then_some(i))
                .collect();
            GraphSpec {
                node_count,
                edges,
                roots,
            }
        })
    })
}

/// Independent reference reachability: breadth-first from the roots over the raw
/// adjacency lists. This is what the collector's mark phase must agree with.
fn reachable_set(spec: &GraphSpec) -> BTreeSet<usize> {
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<usize> = spec.roots.iter().copied().collect();
    for &r in &spec.roots {
        let _ = seen.insert(r);
    }
    while let Some(n) = queue.pop_front() {
        for &target in &spec.edges[n] {
            if seen.insert(target) {
                queue.push_back(target);
            }
        }
    }
    seen
}

/// Materialises a `GraphSpec` into a heap and returns the per-node handles.
fn build(spec: &GraphSpec) -> (Heap<Node>, Vec<Gc<Node>>) {
    let mut heap = Heap::with_capacity(spec.node_count);
    let handles: Vec<Gc<Node>> = (0..spec.node_count)
        .map(|_| heap.alloc(Node { edges: Vec::new() }))
        .collect();
    for (i, targets) in spec.edges.iter().enumerate() {
        let edges = targets.iter().map(|&t| handles[t]).collect();
        heap.get_mut(handles[i]).expect("node is live").edges = edges;
    }
    (heap, handles)
}

proptest! {
    /// After collection, a node resolves iff the reference walk reached it.
    #[test]
    fn collect_keeps_exactly_the_reachable_set(spec in graph_strategy()) {
        let reachable = reachable_set(&spec);
        let (mut heap, handles) = build(&spec);
        let roots: Vec<Gc<Node>> = spec.roots.iter().map(|&i| handles[i]).collect();

        let stats = heap.collect(roots);

        prop_assert_eq!(stats.live, reachable.len());
        prop_assert_eq!(stats.freed, spec.node_count - reachable.len());
        for (i, &handle) in handles.iter().enumerate() {
            prop_assert_eq!(
                heap.get(handle).is_some(),
                reachable.contains(&i),
                "node {} liveness disagreed with reachability",
                i
            );
        }
    }

    /// `len()` equals the survivor count, and `live + freed` equals the original
    /// population — nothing is double-counted or lost.
    #[test]
    fn population_is_conserved(spec in graph_strategy()) {
        let (mut heap, handles) = build(&spec);
        let roots: Vec<Gc<Node>> = spec.roots.iter().map(|&i| handles[i]).collect();

        let before = heap.len();
        let stats = heap.collect(roots);

        prop_assert_eq!(heap.len(), stats.live);
        prop_assert_eq!(stats.live + stats.freed, before);
    }

    /// Every freed handle stays dead across a second collection, and freeing never
    /// disturbs a survivor: idempotence of the survivor set.
    #[test]
    fn second_collection_frees_nothing_new(spec in graph_strategy()) {
        let (mut heap, handles) = build(&spec);
        let roots: Vec<Gc<Node>> = spec.roots.iter().map(|&i| handles[i]).collect();

        let first = heap.collect(roots.clone());
        let live_after_first: Vec<bool> = handles.iter().map(|&h| heap.get(h).is_some()).collect();

        let second = heap.collect(roots);
        let live_after_second: Vec<bool> = handles.iter().map(|&h| heap.get(h).is_some()).collect();

        prop_assert_eq!(second.freed, 0);
        prop_assert_eq!(first.live, second.live);
        prop_assert_eq!(live_after_first, live_after_second);
    }
}

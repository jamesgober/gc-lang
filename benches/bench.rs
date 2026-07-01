//! Criterion benchmarks for the collector's hot paths.
//!
//! Four groups cover the operations a runtime pays for: allocation (fresh slot and
//! reused slot), handle resolution, a single mark-and-sweep over a graph, and a
//! steady-state allocate/collect loop that never grows the backing store.
//!
//! Run with `cargo bench`. Reports land under `target/criterion/`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use gc_lang::{Gc, Heap, Trace, Tracer};

/// A node with arbitrary fan-out — the shape a real object graph takes.
struct Node {
    edges: Vec<Gc<Node>>,
}

impl Trace for Node {
    #[inline]
    fn trace(&self, tracer: &mut Tracer<'_>) {
        for &edge in &self.edges {
            tracer.mark(edge);
        }
    }
}

/// Builds a balanced binary tree of `count` nodes and returns its root. The tree is
/// fully reachable from the root, so a collection rooted at it marks every node.
fn build_tree(heap: &mut Heap<Node>, count: usize) -> Gc<Node> {
    let mut handles: Vec<Gc<Node>> = Vec::with_capacity(count);
    for _ in 0..count {
        handles.push(heap.alloc(Node { edges: Vec::new() }));
    }
    // Wire node i to its two children (2i+1, 2i+2), bottom-up, so index 0 is the root.
    for i in 0..count {
        let (l, r) = (2 * i + 1, 2 * i + 2);
        let mut edges = Vec::new();
        if l < count {
            edges.push(handles[l]);
        }
        if r < count {
            edges.push(handles[r]);
        }
        heap.get_mut(handles[i]).expect("node is live").edges = edges;
    }
    handles[0]
}

fn bench_alloc_fresh(c: &mut Criterion) {
    c.bench_function("alloc_fresh_slot", |b| {
        b.iter_batched(
            Heap::<Node>::new,
            |mut heap| {
                for _ in 0..1024 {
                    let _ = black_box(heap.alloc(Node { edges: Vec::new() }));
                }
                heap
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_alloc_reused(c: &mut Criterion) {
    // Pre-grow the heap and free every slot, so each allocation takes the free-list
    // fast path instead of growing the backing store.
    c.bench_function("alloc_reused_slot", |b| {
        b.iter_batched(
            || {
                let mut heap = Heap::<Node>::with_capacity(1024);
                for _ in 0..1024 {
                    let _ = heap.alloc(Node { edges: Vec::new() });
                }
                let _ = heap.collect(std::iter::empty());
                heap
            },
            |mut heap| {
                for _ in 0..1024 {
                    let _ = black_box(heap.alloc(Node { edges: Vec::new() }));
                }
                heap
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_get(c: &mut Criterion) {
    let mut heap = Heap::<Node>::new();
    let handle = heap.alloc(Node { edges: Vec::new() });
    c.bench_function("get_resolve", |b| {
        b.iter(|| black_box(heap.get(black_box(handle)).is_some()));
    });
}

fn bench_collect(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_reachable_tree");
    for &size in &[1_000usize, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || {
                    let mut heap = Heap::<Node>::with_capacity(size);
                    let root = build_tree(&mut heap, size);
                    (heap, root)
                },
                |(mut heap, root)| {
                    let stats = heap.collect(black_box([root]));
                    black_box(stats)
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_steady_state(c: &mut Criterion) {
    // Allocate a small graph and immediately reclaim all of it, repeatedly. After
    // warm-up this neither grows the backing store nor allocates scratch.
    c.bench_function("steady_state_alloc_collect", |b| {
        let mut heap = Heap::<Node>::with_capacity(64);
        b.iter(|| {
            let a = heap.alloc(Node { edges: Vec::new() });
            let b2 = heap.alloc(Node { edges: Vec::new() });
            let _ = heap.alloc(Node { edges: vec![a, b2] });
            let stats = heap.collect(black_box(std::iter::empty()));
            black_box(stats);
        });
    });
}

criterion_group!(
    benches,
    bench_alloc_fresh,
    bench_alloc_reused,
    bench_get,
    bench_collect,
    bench_steady_state,
);
criterion_main!(benches);

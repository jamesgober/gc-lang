//! A garbage-collected value heap for a small interpreter.
//!
//! This is the shape `gc-lang` is built for: a runtime whose values live on a
//! managed heap and refer to one another by handle. Here the value type is a tiny
//! Lisp-like `Value` — numbers, cons pairs, and closures capturing an environment —
//! and the "roots" are an operand stack, exactly as an interpreter would keep them.
//!
//! Run with: `cargo run --example mini_interpreter`

use gc_lang::{Gc, Heap, Trace, Tracer};

/// The interpreter's runtime value. Compound variants own handles to other values.
enum Value {
    /// A number — a leaf, owns no handles.
    Number(f64),
    /// A cons pair `(car . cdr)` — the building block of lists.
    Pair(Gc<Value>, Gc<Value>),
    /// A closure capturing a list of upvalues from its defining environment.
    Closure { captures: Vec<Gc<Value>> },
}

impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer<'_>) {
        match self {
            Value::Number(_) => {}
            Value::Pair(car, cdr) => {
                tracer.mark(*car);
                tracer.mark(*cdr);
            }
            Value::Closure { captures } => {
                for &capture in captures {
                    tracer.mark(capture);
                }
            }
        }
    }
}

/// Builds the list `(1 2 3)` on the heap and returns a handle to its head pair.
fn build_list(heap: &mut Heap<Value>) -> Gc<Value> {
    let nil = heap.alloc(Value::Number(0.0)); // stand-in for the empty list
    let three = heap.alloc(Value::Number(3.0));
    let two = heap.alloc(Value::Number(2.0));
    let one = heap.alloc(Value::Number(1.0));
    let tail = heap.alloc(Value::Pair(three, nil));
    let mid = heap.alloc(Value::Pair(two, tail));
    heap.alloc(Value::Pair(one, mid))
}

/// Sums the `car` numbers of a proper list, resolving each handle through the heap.
/// Shows the read side of the API: following a graph purely by handle.
fn sum_list(heap: &Heap<Value>, mut node: Gc<Value>) -> f64 {
    let mut total = 0.0;
    while let Some(Value::Pair(car, cdr)) = heap.get(node) {
        if let Some(Value::Number(n)) = heap.get(*car) {
            total += n;
        }
        node = *cdr;
    }
    total
}

fn main() {
    let mut heap: Heap<Value> = Heap::with_capacity(64);

    // The interpreter's operand stack. Whatever it holds is a root.
    let mut stack: Vec<Gc<Value>> = Vec::new();

    // Evaluate an expression that builds a list, and push the result.
    let list = build_list(&mut heap);
    println!("list (1 2 3) sums to {}", sum_list(&heap, list));
    stack.push(list);

    // Build a closure that captures the list, and push it too.
    let closure = heap.alloc(Value::Closure {
        captures: vec![list],
    });
    stack.push(closure);

    // Now allocate a pile of throwaway intermediates — the kind an evaluator
    // produces and discards — without pushing them onto the stack.
    for n in 0..100 {
        let _ = heap.alloc(Value::Number(f64::from(n)));
    }

    println!("before gc: {} objects live", heap.len());

    // Collect: the roots are everything on the operand stack. The list, the closure,
    // and every cell reachable from them survive; the 100 intermediates do not.
    let stats = heap.collect(stack.iter().copied());
    println!(
        "after  gc: {} objects live, {} reclaimed",
        stats.live, stats.freed
    );

    // The rooted values are all still resolvable.
    assert!(heap.get(list).is_some());
    assert!(heap.get(closure).is_some());

    // Pop the closure off the stack, collect again: now only the list is rooted, but
    // the closure captured the list, so the list still lives — the closure itself is
    // gone.
    let _ = stack.pop();
    let stats = heap.collect(stack.iter().copied());
    println!("dropped the closure, reclaimed {} more", stats.freed);
    assert!(heap.get(closure).is_none());
    assert!(heap.get(list).is_some());

    // Clear the stack entirely and collect: nothing is rooted, everything goes.
    stack.clear();
    let stats = heap.collect(stack.iter().copied());
    println!("cleared the stack, reclaimed {} more", stats.freed);
    assert!(heap.is_empty());
}

//! Reachability tracing: the [`Trace`] contract and the [`Tracer`] sink.

extern crate alloc;

use alloc::vec::Vec;

use crate::Gc;

/// Reports the outgoing [`Gc`] edges of a heap object so the collector can follow
/// them.
///
/// A mark-and-sweep collector keeps an object alive by reaching it from a root.
/// "Reaching" means walking the graph of handles one object holds to the next, and
/// only the object itself knows which handles those are — they might be struct
/// fields, vector elements, map values, or buried in an enum variant. `Trace` is
/// how the object hands that list to the collector: [`Heap::collect`] calls
/// [`trace`](Trace::trace) on every object it visits, and the object calls
/// [`Tracer::mark`] once per handle it stores.
///
/// The contract is small but load-bearing:
///
/// - **Mark every owned handle.** A handle you hold but do not mark is invisible to
///   the collector; the object it points at can be swept while you still hold a
///   handle to it. That is not unsound — the stale handle simply resolves to `None`
///   afterwards — but it is almost never what you want. Marking is how you say
///   "keep this alive".
/// - **Marking more than you own is safe.** Extra marks at worst keep an object
///   alive one cycle longer. The collector already ignores handles that do not name
///   a live object, so a stale handle you mark costs nothing.
/// - **Mark only handles into the heap being collected.** [`Tracer::mark`] accepts
///   a handle of any type and reads it as a position in the collecting heap; a
///   handle from another heap does not keep its own object alive and may keep an
///   unrelated one alive. See [`Tracer::mark`] for the details.
/// - **`trace` must not allocate into the heap or mutate the object.** It is a
///   read-only enumeration; it takes `&self` for exactly that reason.
///
/// Leaf objects that hold no handles implement `trace` as an empty body.
///
/// [`Heap::collect`]: crate::Heap::collect
///
/// # Examples
///
/// A cons-cell value type for a small interpreter. Each variant reports the handles
/// it owns and nothing else:
///
/// ```
/// use gc_lang::{Gc, Trace, Tracer};
///
/// enum Value {
///     Nil,
///     Int(i64),
///     Pair(Gc<Value>, Gc<Value>),
///     List(Vec<Gc<Value>>),
/// }
///
/// impl Trace for Value {
///     fn trace(&self, tracer: &mut Tracer<'_>) {
///         match self {
///             // Leaves own no handles.
///             Value::Nil | Value::Int(_) => {}
///             // A pair owns its two children.
///             Value::Pair(car, cdr) => {
///                 tracer.mark(*car);
///                 tracer.mark(*cdr);
///             }
///             // A list owns each element.
///             Value::List(items) => {
///                 for item in items {
///                     tracer.mark(*item);
///                 }
///             }
///         }
///     }
/// }
/// ```
pub trait Trace {
    /// Reports each [`Gc`] handle this object owns by calling [`Tracer::mark`] on it.
    ///
    /// Called by the collector during the mark phase. See the [trait
    /// documentation](Trace) for the contract this method must uphold.
    fn trace(&self, tracer: &mut Tracer<'_>);
}

/// The sink a [`Trace`] implementation reports its outgoing edges to.
///
/// A `Tracer` is handed to [`Trace::trace`] during a collection. Its one job is to
/// receive handles: each call to [`mark`](Tracer::mark) records that the object
/// being traced points at another, so the collector can visit it in turn. It holds
/// a borrow of the collector's work queue and nothing else, so recording an edge is
/// a single push — no allocation on the steady-state path, since the queue is
/// reused across collections.
///
/// You never construct a `Tracer`; the heap builds one and passes it in.
pub struct Tracer<'a> {
    /// The collector's mark-phase work queue, as `(index, generation)` pairs. The
    /// generation travels with the index so a stale handle can be rejected at pop
    /// time rather than silently resolving to whatever now occupies the slot.
    worklist: &'a mut Vec<(u32, u32)>,
}

impl<'a> Tracer<'a> {
    /// Wraps the collector's work queue. Internal: only [`Heap::collect`] builds a
    /// tracer.
    ///
    /// [`Heap::collect`]: crate::Heap::collect
    #[inline]
    pub(crate) fn new(worklist: &'a mut Vec<(u32, u32)>) -> Self {
        Self { worklist }
    }

    /// Records that the object being traced holds `handle`, so the collector will
    /// visit — and thereby keep alive — the object it names.
    ///
    /// Call this once for every handle the object owns. Marking a handle that does
    /// not name a live object is harmless: the collector rejects it when it reaches
    /// the front of the queue.
    ///
    /// # Only handles into the heap being collected do anything
    ///
    /// `mark` is generic: it accepts a `Gc<U>` for **any** `U`, not only handles of
    /// the type the collecting heap stores, and the compiler does not check that
    /// the handle came from that heap. The collector reads every marked handle as
    /// an `(index, generation)` pair in *the heap that is collecting*. So for a
    /// handle that came from somewhere else (a heap of another type, or another
    /// heap of the same type):
    ///
    /// - **It does not keep its own object alive.** Marking a handle into heap `B`
    ///   while heap `A` collects does nothing for `B`. An object in `B` that is
    ///   referenced only from objects in `A` survives `B`'s collections only if the
    ///   runtime passes that handle to `B`'s [`collect`](crate::Heap::collect) as a
    ///   root.
    /// - **It may keep an unrelated object alive.** If its index and generation
    ///   happen to match a live object in `A`, that object is marked — and traced —
    ///   for this collection, though nothing in `A` references it. This is never
    ///   unsound (no handle can dangle; the crate has no `unsafe`), but it is
    ///   over-retention: memory that should have been reclaimed is kept until a
    ///   later collection in which the coincidence does not recur.
    ///
    /// Mark only handles into the heap whose objects you are tracing. Restricting
    /// `mark` to that heap's handle type is a breaking change and is planned for
    /// 2.0 (see the crate's `dev/ROADMAP.md`).
    ///
    /// # Examples
    ///
    /// ```
    /// use gc_lang::{Gc, Trace, Tracer};
    ///
    /// struct Cell {
    ///     next: Option<Gc<Cell>>,
    /// }
    ///
    /// impl Trace for Cell {
    ///     fn trace(&self, tracer: &mut Tracer<'_>) {
    ///         if let Some(next) = self.next {
    ///             tracer.mark(next);
    ///         }
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn mark<T>(&mut self, handle: Gc<T>) {
        self.worklist.push((handle.index(), handle.generation()));
    }
}

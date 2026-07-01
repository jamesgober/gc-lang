//! The typed handle into a garbage-collected heap.

use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// A small, copyable, type-tagged handle to one object in a [`Heap`](crate::Heap).
///
/// A `Gc<T>` is how a garbage-collected object refers to another. It is eight bytes
/// — a slot index plus a generation stamp — so passing one is about as cheap as
/// passing a `u64`, and unlike a `&T` it carries no borrow. That is what lets the
/// runtime build cyclic object graphs: a node stores `Gc<T>` handles to its
/// neighbours, edges point in every direction, and the borrow checker never enters
/// the picture. Resolving a handle is a direct slot lookup through
/// [`Heap::get`](crate::Heap::get) / [`get_mut`](crate::Heap::get_mut).
///
/// The generation stamp is what makes a handle safe to hold across a collection. A
/// slot's generation advances every time the slot is reclaimed and reused, so a
/// handle to an object that was collected no longer matches the object now living
/// in that slot: it resolves to `None` rather than silently aliasing an unrelated
/// value. A handle is therefore never a dangling pointer — at worst it is a stale
/// handle that reads as absent.
///
/// The `T` tag is compile-time only and occupies no space: it stops a `Gc<Value>`
/// from being resolved against a `Heap<Node>`. `Gc<T>` is `Copy`, `Eq`, `Ord`, and
/// `Hash` for **every** `T` — the tag never adds a bound — so it works as a map key
/// regardless of what it points at. There is no public constructor: a `Gc` can only
/// come from [`Heap::alloc`](crate::Heap::alloc) / [`try_alloc`](crate::Heap::try_alloc).
///
/// # Examples
///
/// ```
/// use gc_lang::{Heap, Trace, Tracer};
///
/// struct Node;
/// impl Trace for Node {
///     fn trace(&self, _: &mut Tracer<'_>) {}
/// }
///
/// let mut heap = Heap::new();
/// let handle = heap.alloc(Node);
///
/// // It is Copy and eight bytes wide, whatever it points at.
/// let also = handle;
/// assert_eq!(handle, also);
/// assert_eq!(core::mem::size_of_val(&handle), 8);
/// ```
pub struct Gc<T> {
    index: u32,
    generation: u32,
    /// Compile-time type tag. `fn() -> T` keeps `Gc<T>` `Copy`, `Send`, and `Sync`
    /// for any `T`, and covariant in `T`, without ever borrowing or owning a `T`.
    marker: PhantomData<fn() -> T>,
}

impl<T> Gc<T> {
    /// Wraps a raw `(index, generation)` pair. Internal: only a heap mints handles,
    /// so the type tag always matches the heap the handle indexes.
    #[inline]
    pub(crate) const fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            marker: PhantomData,
        }
    }

    /// Returns the slot index this handle addresses.
    #[inline]
    pub(crate) const fn index(self) -> u32 {
        self.index
    }

    /// Returns the generation stamp this handle was minted with.
    #[inline]
    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }
}

// The trait impls are written by hand rather than derived: a derive would bound
// each impl on `T` (e.g. `T: Clone`), but a handle must be `Copy`, comparable, and
// hashable for every `T`, since `T` is only a compile-time tag.

impl<T> Clone for Gc<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Gc<T> {}

impl<T> PartialEq for Gc<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for Gc<T> {}

impl<T> PartialOrd for Gc<T> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Gc<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        // Index first, then generation: handles into the same slot from different
        // eras stay distinct and adjacent under an ordering.
        self.index
            .cmp(&other.index)
            .then(self.generation.cmp(&other.generation))
    }
}

impl<T> Hash for Gc<T> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<T> fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `index@generation` reads as "the object in slot N, era G".
        write!(f, "Gc({}@{})", self.index, self.generation)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    extern crate alloc;
    use alloc::collections::BTreeSet;
    use alloc::format;

    use crate::{Heap, Trace, Tracer};

    struct Leaf;
    impl Trace for Leaf {
        fn trace(&self, _: &mut Tracer<'_>) {}
    }

    #[test]
    fn test_handle_traits_do_not_depend_on_the_tag() {
        // A tag that is neither `Clone` nor `Eq` must not stop `Gc` from being
        // `Copy`, `Eq`, and `Debug` — the tag is compile-time only.
        struct NotClone;
        impl Trace for NotClone {
            fn trace(&self, _: &mut Tracer<'_>) {}
        }
        let mut heap = Heap::<NotClone>::new();
        let handle = heap.alloc(NotClone);
        let copy = handle; // Copy
        assert_eq!(handle, copy); // Eq
        assert!(format!("{handle:?}").starts_with("Gc")); // Debug
    }

    #[test]
    fn test_distinct_allocations_have_distinct_handles() {
        let mut heap = Heap::new();
        let handles: BTreeSet<_> = (0..16).map(|_| heap.alloc(Leaf)).collect();
        assert_eq!(handles.len(), 16); // all distinct, usable as ordered-set keys
    }

    #[test]
    fn test_handle_is_eight_bytes_for_any_element() {
        assert_eq!(core::mem::size_of::<crate::Gc<u8>>(), 8);
        assert_eq!(core::mem::size_of::<crate::Gc<[u128; 4]>>(), 8);
    }
}

//! The error type returned by the fallible allocation path.

use core::fmt;

/// The reason a value could not be allocated into a [`Heap`](crate::Heap).
///
/// The heap addresses its slots with a 32-bit index, so it can hold up to
/// `u32::MAX + 1` distinct slots over its lifetime. Slots are reused as objects
/// are reclaimed, so this ceiling counts *simultaneously live plus never-yet-freed*
/// slots, not total allocations — a program that allocates and collects in a steady
/// loop never approaches it. Reaching the ceiling is the one recoverable failure an
/// allocation can hit; [`Heap::try_alloc`] reports it through this type instead of
/// aborting, so a runtime driving the heap from untrusted input can fail cleanly
/// rather than crash.
///
/// The enum is `#[non_exhaustive]`: a later phase may add a second failure mode
/// (for example, a per-heap byte budget), and a `match` on this type must already
/// account for it.
///
/// [`Heap::try_alloc`]: crate::Heap::try_alloc
///
/// # Examples
///
/// ```
/// use gc_lang::{GcError, Heap, Trace, Tracer};
///
/// struct Leaf;
/// impl Trace for Leaf {
///     fn trace(&self, _: &mut Tracer<'_>) {}
/// }
///
/// // The fallible path returns this type; the happy path yields a handle.
/// let mut heap: Heap<Leaf> = Heap::new();
/// let handle = heap.try_alloc(Leaf).expect("the first slot is always available");
/// assert!(heap.get(handle).is_some());
/// # let _ = GcError::CapacityExhausted;
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GcError {
    /// The heap's slot space is full: it already addresses `u32::MAX + 1` slots
    /// and cannot represent another handle.
    ///
    /// This is unreachable for any realistic workload — it takes more than four
    /// billion slots that were never reclaimed — but it is reported rather than
    /// ignored so the limit is a defined boundary, never a silent wrap. When it
    /// does occur, run a collection to reclaim dead slots before allocating again.
    CapacityExhausted,
}

impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CapacityExhausted => {
                f.write_str("heap is full: cannot address beyond u32::MAX slots")
            }
        }
    }
}

impl core::error::Error for GcError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    extern crate alloc;
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn test_capacity_exhausted_display_is_actionable() {
        let text = GcError::CapacityExhausted.to_string();
        assert!(text.contains("u32::MAX"), "{text}");
    }

    #[test]
    fn test_error_is_copy_and_equatable() {
        let a = GcError::CapacityExhausted;
        let b = a;
        assert_eq!(a, b);
    }
}

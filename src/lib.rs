//! # gc_lang
//!
//! A tracing garbage collector for interpreted-language runtimes.
//!
//! `gc-lang` gives a runtime one focused thing: a heap it can allocate objects into
//! and periodically sweep, reclaiming everything no longer reachable — cycles
//! included. Allocate a value into a [`Heap`] and get back a [`Gc`] handle: a small,
//! `Copy`, generation-stamped reference that objects store to point at one another.
//! When the runtime hands its roots to [`Heap::collect`], the collector traces the
//! graph those roots reach and frees the rest.
//!
//! It is a mark-and-sweep collector, and it is entirely safe — the crate is
//! `#![forbid(unsafe_code)]`. Two design choices carry that:
//!
//! - **Reachability, not reference counting.** Collection is decided by tracing from
//!   roots, so unreachable cycles are reclaimed. A runtime can build arbitrary
//!   object graphs — doubly-linked structures, back-edges, self-references — without
//!   leaking them.
//! - **Handles, not pointers.** A [`Gc`] is an index plus a generation stamp, not an
//!   address. Objects wire to each other by handle, so the graph never fights the
//!   borrow checker, and a handle to a collected object resolves to `None` instead
//!   of dangling. There is no use-after-free to have.
//!
//! It owns object storage and reclamation only — no value representation, no
//! interpreter, no scheduling of when to collect. The runtime decides what its roots
//! are and when a collection is worth its cost.
//!
//! ## The `Trace` contract
//!
//! An object type becomes collectable by implementing [`Trace`]: during the mark
//! phase the collector calls [`Trace::trace`], and the object reports each [`Gc`]
//! handle it owns by calling [`Tracer::mark`]. Marking a handle keeps its object
//! alive; a handle you hold but never mark is treated as unreachable. Leaf objects
//! that own no handles implement `trace` as an empty body.
//!
//! ## Quickstart
//!
//! A two-object graph: reclaim the unreachable one, keep the rooted one.
//!
//! ```
//! use gc_lang::{Gc, Heap, Trace, Tracer};
//!
//! // The runtime's object type. It owns handles to other objects; `trace` reports them.
//! enum Value {
//!     Number(f64),
//!     Pair(Gc<Value>, Gc<Value>),
//! }
//!
//! impl Trace for Value {
//!     fn trace(&self, tracer: &mut Tracer<'_>) {
//!         if let Value::Pair(a, b) = self {
//!             tracer.mark(*a);
//!             tracer.mark(*b);
//!         }
//!     }
//! }
//!
//! let mut heap = Heap::new();
//! let one = heap.alloc(Value::Number(1.0));
//! let two = heap.alloc(Value::Number(2.0));
//! let pair = heap.alloc(Value::Pair(one, two));
//! let unused = heap.alloc(Value::Number(3.0));
//! assert_eq!(heap.len(), 4);
//!
//! // Collect with `pair` as the only root. `pair`, `one`, and `two` are reachable;
//! // `unused` is not.
//! let stats = heap.collect([pair]);
//! assert_eq!(stats.freed, 1);
//! assert!(heap.get(unused).is_none());
//! assert!(heap.get(one).is_some());
//! ```
//!
//! ## `no_std`
//!
//! The crate is `no_std` by default-compatible: it needs only `alloc`, and the
//! `std` feature (on by default) links the standard library without changing the
//! public surface. Disable default features to build in a `no_std` environment.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(unused_must_use)]
#![deny(unused_results)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::unreachable)]

extern crate alloc;

mod error;
mod handle;
mod heap;
mod trace;

pub use error::GcError;
pub use handle::Gc;
pub use heap::{CollectStats, Heap};
pub use trace::{Trace, Tracer};

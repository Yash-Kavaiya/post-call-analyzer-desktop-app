//! Sharing call metrics between machines.
//!
//! A supervisor with agents on ten laptops needs one dashboard. This is how
//! those ten corpora reach it — and, just as importantly, what they do not
//! send. See [`record`] for the argument: **metrics travel, content does not.**
//!
//! The shape is deliberately small:
//!
//! - A **client** projects its corpus into [`SyncRecord`]s, pushes what has
//!   changed since a watermark, and pulls what the peer has that it has not.
//! - A **server** holds one corpus per tenant, keyed by bearer token, and
//!   answers those two calls.
//!
//! There is no conflict resolution because there are no conflicts: a record is
//! owned by the machine that produced the call, keyed by an id unique within a
//! tenant, and a re-push replaces rather than merges.
//!
//! ## What this is not
//!
//! The HTTP layer is plain `std::net` with no TLS. **Run it behind a reverse
//! proxy that terminates TLS** — the transport carries scores rather than
//! transcripts, but scores about identifiable people over plaintext HTTP on an
//! untrusted network is still wrong. [`server`] refuses to bind a non-loopback
//! address unless told explicitly, so the insecure case has to be chosen.

pub mod client;
pub mod http;
pub mod record;
pub mod server;

pub use client::{Direction, SyncOutcome, SyncPeer, Transport};
pub use http::HttpTransport;
pub use record::{FindingMetric, PullResponse, PushRequest, PushResponse, SyncRecord};
pub use server::{ServerConfig, TokenMap};

/// Largest request body the server will read, in bytes.
///
/// A sync batch of 500 records is roughly 150 KB, so 8 MB is generous. The
/// point is that it is bounded: without a cap, a single request can drive the
/// process out of memory.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Records per batch. Bounded so one push cannot hold the mutex for a minute.
pub const BATCH: usize = 500;

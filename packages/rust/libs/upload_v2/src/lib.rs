//! Server upload with negotiated, bounded protocol v2 batches.

mod dispatch;
mod group;
mod known;
mod outcome;
mod phases;
mod transfer;

pub use dispatch::ServerBackend;

//! Shared schema for hqc. Source of truth: docs/SPEC.md §3 (Event), §4 (event types),
//! §5 (worker <-> hub frames), §6 (client <-> hub: `/api/ws` frames in [`client_frame`],
//! REST bodies in [`api`]). Keep this crate dependency-light: it is linked into the hub,
//! the worker and (via generated TS types later) the client.

pub mod api;
pub mod client_frame;
pub mod event;
pub mod frame;

/// Worker <-> hub protocol version (SPEC §5.1). Bump on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;

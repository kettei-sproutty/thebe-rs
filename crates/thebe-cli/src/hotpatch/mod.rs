//! Internal building blocks for Thebe's future first-party hotpatch engine.
//!
//! The design target lives in `docs/hotpatch-engine.md`. These modules are
//! intentionally internal until the runtime path is ready for a user-facing CLI
//! flag.

pub(crate) mod classify;
pub(crate) mod browser;
pub(crate) mod orchestrator;
pub(crate) mod runtime;
pub(crate) mod session;

//! Typed side-chat policy shared with clients that do not depend on core.
//!
//! The shared crate owns the fragment so clients can checkpoint the same policy.

pub use codex_context_fragments::SideConversationPolicy;

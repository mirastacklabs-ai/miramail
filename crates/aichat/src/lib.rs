//! MiraMail AI chat engine. See docs/AI_CHAT_PLAN.md (owner decisions at the end).
//! Modules are filled in by separate tasks; keep this file the only place that declares them.
pub mod types;
pub mod sse;
pub mod provider;
pub mod http;
pub mod store;
pub mod budget;
pub mod context;
pub mod engine;
pub mod registry;
pub mod envelope;
pub mod redact;
pub mod tools;
pub mod proposals;
pub mod drafts;
pub mod rules;
pub mod autoreply;
pub mod cal;
pub mod calexec;
pub mod tone;

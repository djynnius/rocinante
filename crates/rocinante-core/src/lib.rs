//! Rocinante core: agent loop, tools, permissions, sessions, context, skills.
//!
//! This crate is a pure library with an event-stream API. Frontends (REPL,
//! TUI, future HTTP server) talk to it over channels only — no terminal or
//! HTTP code lives here.

pub mod agent;
pub mod brainbox;
pub mod config;
pub mod context;
pub mod interval;
pub mod lsp;
pub mod mcp;
pub mod permissions;
pub mod prompt;
pub mod provider_factory;
pub mod session;
pub mod skills;
pub mod state;
pub mod tools;

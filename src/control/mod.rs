//! Managed execution authority. Projects' files are views, never a second scheduler.
pub mod api;
pub mod broker_policy;
pub mod cli;
pub mod coordinator;
pub mod coordinator_launch;
mod delivery;
pub mod executor;
pub mod git_pipeline;
pub mod identity;
pub mod lifecycle;
mod live_memory;
pub mod model;
pub mod node;
pub mod process;
mod reception;
mod recovery;
pub mod runtime;
pub mod service;
pub mod store;
pub mod transport;
pub mod verification;
pub mod worker;

#[cfg(test)]
mod tests;

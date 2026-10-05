//! Managed execution authority. Projects' files are views, never a second scheduler.
pub mod api;
mod artifact_inputs;
mod artifact_review;
pub mod artifacts;
pub mod broker_policy;
mod cancellation_recovery;
mod check_receipts;
mod cleanup;
pub mod cli;
pub mod coordinator;
pub mod coordinator_launch;
mod delivery;
mod event_recovery;
pub mod executor;
pub mod git_pipeline;
pub mod herdr_events;
pub mod identity;
mod kernel_process;
pub mod lifecycle;
mod live_memory;
pub mod model;
pub mod node;
mod node_lifecycle;
pub mod pagination;
pub mod process;
mod process_recovery;
mod reception;
mod recovery;
mod registration_recovery;
mod run_profiles;
pub mod runtime;
pub mod service;
pub mod service_setup;
pub mod store;
pub mod transport;
pub mod verification;
pub mod worker;

#[cfg(test)]
mod tests;

pub mod receipt;
pub mod record;

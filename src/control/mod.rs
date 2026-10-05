//! Managed execution authority. Projects' files are views, never a second scheduler.
pub mod api;
pub mod cli;
pub mod executor;
pub mod git_pipeline;
pub mod identity;
pub mod lifecycle;
pub mod model;
pub mod node;
pub mod process;
pub mod runtime;
pub mod service;
pub mod store;
pub mod transport;
pub mod verification;
pub mod worker;

#[cfg(test)]
mod tests;

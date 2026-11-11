//! The `/v1/agents` client shared by Juno's Agents and CodeCor apps: the `Api` trait with the account's and sample mode's
//! implementations, the proxy's wire models, and the store kept live by the stream's events.

pub mod agent;
pub mod api;
pub mod diff;
pub mod model;
pub mod pages;
pub mod store;
pub mod stream;
pub mod ui;

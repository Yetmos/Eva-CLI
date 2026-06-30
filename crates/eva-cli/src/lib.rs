//! User-facing CLI boundary.

pub mod adapter;
pub mod agent;
pub mod capability;
pub mod emit;
pub mod inspect;
pub mod run;

/// Entry point used by the root binary shim.
pub fn run() {
    run::run();
}

//! `gt-validate` — a CLI demo of [`groundtruth_validator`] on arbitrary
//! CSV input. The binary is in `main.rs`; this crate exposes everything
//! as a library too so integration tests can drive it programmatically.

pub mod args;
pub mod csv_input;
pub mod output;
pub mod report;
pub mod runner;

pub use args::{Args, OutputFormat};
pub use report::Report;
pub use runner::{run, run_with_reader};

//! Pure-Rust implementation support for `specialized(powerpoint)`.
//!
//! Presentation construction, OOXML animation/transition injection, package
//! serialization, validation, inspection, and atomic writes all run in-process.
//! No Python interpreter or Python packages are required at runtime.

mod native;

pub use native::{append, create, edit, inspect, open_save};

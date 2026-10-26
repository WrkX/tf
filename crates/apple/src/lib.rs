//! aufrufe hier können blockieren; nie vom haupt-thread aufrufen.

mod ffi;
pub mod foundation_models;
pub mod speech;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

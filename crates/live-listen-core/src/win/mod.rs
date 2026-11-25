//! Windows integration: the input hook thread, text injection and target-window checks.

#[cfg(windows)]
pub mod cards_pipe;
pub mod focus;
pub mod hook;
pub mod input;

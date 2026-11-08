//! Core of Live Listen: settings, shortcuts, audio capture, speech providers, dictation, captions
//! and spoken-note cards.
//!
//! The WinUI shell and the CLI are thin adapters over this crate.

pub mod audio;
pub mod captions;
pub mod cards;
pub mod config;
pub mod dictation;
pub mod engine;
pub mod hotkey;
pub mod paths;
pub mod protocol;
pub mod provider;
pub mod secrets;
pub mod win;

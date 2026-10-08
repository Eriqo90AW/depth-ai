//! depth — on-device transcription of Windows desktop audio.
//!
//! The crate is split so the pipeline can be driven from the tray binary, the diagnostic CLI
//! modes, and the tests alike:
//!
//! - [`capture`] — WASAPI loopback of the default output device, resampled to 16 kHz mono
//! - [`vad`] — energy gate that turns the stream into utterances
//! - [`engine`] — Whistle (English) and Whisper (Indonesian) behind one trait
//! - [`recording`] — recording identities, events, documents and legacy imports
//! - [`pipeline`] — threads, queues and status wiring
//! - [`shell`] — opening transcripts, folders and the config file
//! - [`gui`] — Slint windows and shared application controller

pub mod capture;
pub mod config;
pub mod engine;
pub mod hotkey;

pub mod live;
pub mod logging;
pub mod pipeline;

pub mod shell;
pub mod vad;

pub mod writer;

#[cfg(feature = "tray")]
pub mod appearance;
pub mod clipboard;
#[cfg(feature = "tray")]
pub mod gui;
pub mod recording;
#[cfg(feature = "tray")]
pub mod tray;

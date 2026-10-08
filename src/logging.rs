//! Minimal file logger.
//!
//! The app runs in the tray with no console, so the log file is the only diagnostic surface.
//! Kept dependency-free on purpose: one append-only file with a size cap.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Local;

/// Rotate once the log passes this size, keeping a single `.old` file.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// A single append-only log file, safe to share across threads.
pub struct Logger {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl Logger {
    /// Open (or create) the log, rotating it if it has grown too large.
    pub fn new(path: impl Into<PathBuf>) -> Logger {
        let path = path.into();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::metadata(&path)
            .map(|m| m.len() > MAX_BYTES)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
        Logger {
            path,
            file: Mutex::new(None),
        }
    }

    /// A logger that discards everything, for tests and `--check`.
    pub fn disabled() -> Logger {
        Logger {
            path: PathBuf::new(),
            file: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_line(&self, level: &str, message: &str) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        let line = format!(
            "[{}] {:<5} {message}\n",
            Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
            level
        );
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.is_none() {
            *guard = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(file) = guard.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.write_line("INFO", message.as_ref());
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.write_line("WARN", message.as_ref());
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.write_line("ERROR", message.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_logger_is_silent() {
        let log = Logger::disabled();
        log.info("nothing should happen");
        assert!(log.path().as_os_str().is_empty());
    }
}

use crate::local::unix_ms;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize)]
struct Entry<'a> {
    time_unix_ms: u64,
    level: &'a str,
    category: &'a str,
    message: &'a str,
}

pub struct Logger {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Logger {
    pub fn new(root: &std::path::Path) -> Self {
        Self {
            path: root.join("logs/isaaccloud.ndjson"),
            lock: Mutex::new(()),
        }
    }

    pub fn log(&self, level: &str, category: &str, message: &str) {
        // Tokens are never accepted as logger fields. This last-resort guard
        // also rejects JWT-shaped strings if an upstream error includes one.
        let safe_message = if message.matches('.').count() >= 2 && message.len() > 80 {
            "sensitive-looking value redacted"
        } else {
            message
        };
        let Ok(_guard) = self.lock.lock() else { return };
        let Some(parent) = self.path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        if self
            .path
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
        {
            let rotated = self.path.with_extension("ndjson.1");
            let _ = fs::remove_file(&rotated);
            let _ = fs::rename(&self.path, rotated);
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let entry = Entry {
            time_unix_ms: unix_ms(),
            level,
            category,
            message: safe_message,
        };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = writeln!(file, "{line}");
        }
    }
}

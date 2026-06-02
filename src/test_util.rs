//! Test-only utilities shared across `#[cfg(test)]` modules.
//!
//! Process-global because `config::save` and `config::load` rely on
//! the `$HOME` environment variable, which is process-global state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// One global Mutex that serialises every test which touches
/// `config::save` / `config::load`. Holding the lock for the full
/// duration of a test ensures no other test can change `$HOME` in
/// the middle of a save/load pair.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global test lock, recovering from poisoning so a
/// panic in one test does not break the rest.
pub fn lock() -> MutexGuard<'static, ()> {
    match TEST_LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Counter used to generate unique per-test temp directory names.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a unique temp directory and return its path.
pub fn unique_tempdir(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "{}-{}-{}-{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        n,
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// RAII guard that holds the global test lock and sets `$HOME` to a
/// fresh temp dir for the test's duration. On drop it restores
/// `$HOME` and removes the temp dir.
pub struct HomeGuard {
    _g: MutexGuard<'static, ()>,
    prev: Option<String>,
    dir: PathBuf,
}

impl HomeGuard {
    pub fn new(prefix: &str) -> Self {
        let g = lock();
        let dir = unique_tempdir(prefix);
        let prev = std::env::var("HOME").ok();
        // Safety: we hold TEST_LOCK so no other thread can race us
        // between set_var and the test's save/load calls.
        unsafe { std::env::set_var("HOME", &dir) };
        Self { _g: g, prev, dir }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

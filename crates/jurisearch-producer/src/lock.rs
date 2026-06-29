//! The single `core` update lock around all DB-mutating producer work.
//!
//! Both fetch groups write the SAME `core` outbox and the package builder selects rows by corpus (not by
//! source/run), so ingest→enrich→embed→`producer_cycle("core")` MUST be serialized. The lock is an
//! advisory flock with a BOUNDED wait: a closely-spaced run waits up to a timeout for the holder to
//! finish (then proceeds and still publishes its work); only if the wait times out does it surface a
//! distinct `skipped-lock-held` signal — never a silent no-op. Only the pure network download may run
//! outside this lock (it makes no DB writes).

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::error::ProducerError;

/// The held update-core lock. Releasing happens on drop (the flock is tied to the open file handle).
#[derive(Debug)]
pub struct UpdateLock {
    _file: File,
    name: String,
}

impl UpdateLock {
    /// The lock's display name (e.g. `update-core`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Non-blocking probe: is the `update-core` lock currently held by some run? Used by `status --json` to
/// report contention WITHOUT taking the lock. Opening + `try_lock` + drop releases immediately if free,
/// so this never blocks a real run; a held lock surfaces as `true`. A missing lock file (never run) is
/// `false`.
#[must_use]
pub fn is_update_lock_held(state_dir: &Path) -> bool {
    let path = state_dir.join("update-core.lock");
    let Ok(file) = OpenOptions::new().write(true).read(true).open(&path) else {
        return false; // no lock file yet ⇒ never contended
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// Acquire the `update-core` lock under `state_dir`, waiting up to `max_wait` for a concurrent holder.
/// Returns [`ProducerError::LockHeld`] (class `skipped-lock-held`) if the wait times out.
pub fn acquire_update_lock(
    state_dir: &Path,
    max_wait: Duration,
) -> Result<UpdateLock, ProducerError> {
    std::fs::create_dir_all(state_dir).map_err(|source| ProducerError::Io {
        path: state_dir.to_path_buf(),
        source,
    })?;
    let path = state_dir.join("update-core.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&path)
        .map_err(|source| ProducerError::Io {
            path: path.clone(),
            source,
        })?;

    let deadline = Instant::now() + max_wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(UpdateLock {
                    _file: file,
                    name: "update-core".to_owned(),
                });
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => {
                return Err(ProducerError::LockHeld {
                    lock: "update-core".to_owned(),
                    waited_secs: max_wait.as_secs(),
                });
            }
        }
    }
}

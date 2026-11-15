//! Managed scratch space for crash-safe temporary files.
//!
//! The engine writes a few files in-place — `doom.cfg` today, savegames
//! later — and a crash in the middle of a write leaves a torn file
//! behind. This module routes those writes through a managed scratch
//! area built on the `cachegirl` crate:
//!
//! - the bytes are written to a temporary file tracked by a `cachegirl`
//!   manager (namespaces, registry, crash recovery),
//! - the file handle is `sync_all`ed so the data is durable before the
//!   commit point,
//! - `std::fs::rename` swaps it into place — atomic within one
//!   filesystem.
//!
//! Atomicity only holds on a single filesystem, so the scratch file must
//! live next to the target, not in a global temp dir. Every scratch root
//! therefore sits in a dedicated `.doom-rs-scratch` directory inside the
//! target's parent (see [`SCRATCH_DIR_NAME`]), and `cachegirl` fills it
//! with `<root>/<instance>/<namespace>/<name>` entries.
//!
//! Crash recovery runs on every open ([`ScratchDir::open`] /
//! [`atomic_write`] / [`sweep_stale`]): stale instance directories left
//! by crashed runs are reclaimed before anything new is created, so
//! debris from a half-finished write does not pile up.
//!
//! Nothing here is `unsafe`; all handles are `Arc`-based.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cachegirl::{FileResource, TempManager};
use tracing::warn;

use crate::errors::{EngineError, EngineResult};

/// Directory name used for scratch data next to a target file.
///
/// Lives directly under the target's parent so `rename` in
/// [`atomic_write`] stays on one filesystem.
pub const SCRATCH_DIR_NAME: &str = ".doom-rs-scratch";

/// Namespace used for in-flight atomic writes.
const ATOMIC_WRITE_NAMESPACE: &str = "init";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_name() -> String {
    let pid = std::process::id();
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("atomic-{pid}-{seq}-{nanos}.tmp")
}

fn scratch_err(root: &Path) -> impl Fn(cachegirl::Error) -> EngineError + '_ {
    move |source| EngineError::Scratch {
        path: root.to_path_buf(),
        source,
    }
}

/// A managed scratch directory, backed by one `cachegirl` manager.
///
/// The manager is opened with `create_if_missing` and
/// `cleanup_on_start`, so opening a scratch root always runs the crash
/// recovery pass over it first. Dropping the value without calling
/// [`shutdown`](Self::shutdown) leaves the instance lock file behind on
/// purpose — that is exactly what the recovery pass on the next open is
/// designed to detect (a lock naming a dead pid means the previous run
/// crashed). Call [`shutdown`](Self::shutdown) for a clean close.
pub struct ScratchDir {
    manager: TempManager,
}

impl ScratchDir {
    /// Open or create the scratch root at `root`.
    ///
    /// Stale instance directories from crashed runs under `root` are
    /// reclaimed as part of opening it.
    pub fn open(root: &Path) -> EngineResult<Self> {
        let manager = TempManager::builder()
            .root(root)
            .create_if_missing(true)
            .cleanup_on_start(true)
            .build()
            .map_err(scratch_err(root))?;
        Ok(Self { manager })
    }

    /// The default scratch root for artifacts that do not target a
    /// specific file: `<system temp dir>/doom-rs`.
    pub fn default_root() -> PathBuf {
        std::env::temp_dir().join("doom-rs")
    }

    /// Open the scratch root at [`default_root`](Self::default_root).
    pub fn open_default() -> EngineResult<Self> {
        Self::open(&Self::default_root())
    }

    /// Open the scratch root belonging to `dir`
    /// (i.e. `dir/[SCRATCH_DIR_NAME](SCRATCH_DIR_NAME)`).
    ///
    /// Use this when artifacts will be renamed within `dir` later — the
    /// temp file must share the target's filesystem.
    pub fn for_dir(dir: &Path) -> EngineResult<Self> {
        Self::open(&dir.join(SCRATCH_DIR_NAME))
    }

    /// Track a new empty file in `namespace` under name `name`.
    ///
    /// The file is created on disk and registered; the caller is
    /// expected to write to it and then either rename it into place or
    /// hand it to `sweep_stale` to be reclaimed later. Dropping the
    /// handle does not delete the file (the resource is `Managed`).
    pub fn create_file(&self, namespace: &str, name: &str) -> EngineResult<FileResource> {
        let root = self.manager.root().to_path_buf();
        let ns = self
            .manager
            .namespace(namespace)
            .map_err(scratch_err(&root))?;
        ns.create_file(name).map_err(scratch_err(&root))
    }

    /// Finish the scratch session and remove this instance's lock file.
    ///
    /// Logged-and-ignored on failure by the caller when this runs as
    /// cleanup after another failure; callers that only care about the
    /// clean case should propagate the error.
    pub fn shutdown(self) -> EngineResult<()> {
        let root = self.manager.root().to_path_buf();
        self.manager
            .shutdown()
            .map(|_report| ())
            .map_err(scratch_err(&root))
    }
}

/// Write `bytes` to `path` atomically.
///
/// The payload is staged in a temporary, manager-tracked file inside
/// `<path-parent>/.doom-rs-scratch/`, synced to disk and then renamed
/// onto `path`. A crash mid-write leaves the previous `path` contents
/// intact and the staged fragment is reclaimed by the recovery pass on
/// a later call (best case immediately, via the release attempts below;
/// worst case on the next open of the scratch root).
///
/// Any filesystem error aborts the write before the rename, so an
/// existing `path` is never clobbered by a partial payload.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> EngineResult<()> {
    // rename is only atomic within one filesystem; keep the staged file
    // next to the target instead of in a global temp dir
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // do not let the scratch root creation silently mkdir -p a missing
    // target parent — if the target's directory is gone, that is an
    // error worth reporting, not a cue to build a directory tree
    if !parent.is_dir() {
        return Err(EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "parent directory {} for target {} does not exist or is not a directory",
                parent.display(),
                path.display()
            ),
        )));
    }
    let root = parent.join(SCRATCH_DIR_NAME);
    let scratch = ScratchDir::open(&root)?;

    let result = atomic_write_staged(&scratch, path, bytes);

    // never skip shutdown, even on the error path: it removes our lock
    // file, otherwise every failed save looks like a crashed run to the
    // recovery pass on the next call
    if let Err(e) = scratch.shutdown() {
        warn!(
            "scratch: shutdown after writing {} failed: {e}",
            path.display()
        );
    }
    result
}

fn atomic_write_staged(scratch: &ScratchDir, target: &Path, bytes: &[u8]) -> EngineResult<()> {
    let root = scratch.manager.root().to_path_buf();
    let ns = scratch
        .manager
        .namespace(ATOMIC_WRITE_NAMESPACE)
        .map_err(scratch_err(&root))?;
    let temp = ns
        .create_file(&unique_temp_name())
        .map_err(scratch_err(&root))?;
    let temp_path = temp.path().to_path_buf();

    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        release_best_effort(&ns, temp.id(), &temp_path, target);
        return Err(EngineError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "writing scratch file {} for target {}: {e}",
                temp_path.display(),
                target.display()
            ),
        )));
    }

    if let Err(e) = fs::rename(&temp_path, target) {
        release_best_effort(&ns, temp.id(), &temp_path, target);
        return Err(EngineError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "rename {} onto {}: {e}",
                temp_path.display(),
                target.display()
            ),
        )));
    }

    // the file lives outside the scratch root now, so the registry's
    // release path may report "already gone" instead of removing
    // anything — which is fine. Either way the payload is already the
    // new target; the worst case is a lingering in-memory record that
    // dies with the manager, never data loss. Hence: log and move on,
    // deliberately.
    release_best_effort(&ns, temp.id(), &temp_path, target);
    Ok(())
}

/// Unregister `id`, deleting the staged file from disk if it is still
/// there.
///
/// Used on error paths (so nothing is left behind) and after the rename
/// (so the bookkeeping does not linger). Failure is not actionable by
/// the caller, hence warn-and-continue.
fn release_best_effort(
    ns: &cachegirl::Namespace,
    id: &cachegirl::ResourceId,
    temp_path: &Path,
    target: &Path,
) {
    if let Err(e) = ns.release(id) {
        warn!(
            "scratch: could not release staged file {} (target {}): {e}",
            temp_path.display(),
            target.display()
        );
    }
}

/// Public helper for longer-lived scratch artifacts (tests, tooling).
///
/// Creates a tracked, empty file `name` inside
/// `<dir>/<instance>/<namespace>/` — `dir` is the scratch root itself —
/// and returns the handle. The artifact persists until reclaimed by
/// [`sweep_stale`] or removed by the caller; dropping the handle does
/// not delete it. Use [`SCRATCH_DIR_NAME`]-style roots (or
/// [`ScratchDir::default_root`]) so the artifact can be swept later.
pub struct ScratchTemp;

impl ScratchTemp {
    /// Create `name` in `namespace` under the scratch root `dir`
    /// (created if missing).
    ///
    /// The manager's lock is removed before returning; the file itself
    /// stays on disk until swept or removed out of band.
    pub fn create_temp(dir: &Path, namespace: &str, name: &str) -> EngineResult<FileResource> {
        let scratch = ScratchDir::open(dir)?;
        let result = scratch.create_file(namespace, name);
        if let Err(e) = scratch.shutdown() {
            warn!(
                "scratch: shutdown after creating {namespace}/{name} in {} failed: {e}",
                dir.display()
            );
        }
        result
    }
}

/// Reclaim stale scratch state under `dir` and return how many entries
/// were removed.
///
/// Counts both crash-recovery removals (stale instance directories from
/// interrupted runs, reclaimed when the root is opened) and removals
/// from one `cleanup_expired` pass. Opportunistic housekeeping for
/// engine startup: failures are logged and reported as `0` instead of
/// aborting — a broken scratch root must not brick game launch.
pub fn sweep_stale(dir: &Path) -> usize {
    let scratch = match ScratchDir::open(dir) {
        Ok(scratch) => scratch,
        Err(e) => {
            warn!("scratch: sweep of {} failed to open: {e}", dir.display());
            return 0;
        }
    };

    let recovered = scratch
        .manager
        .recovery_report()
        .map(|report| report.removed as usize)
        .unwrap_or(0);

    let cleaned = match scratch.manager.cleanup_expired() {
        Ok(report) => report.removed as usize,
        Err(e) => {
            warn!("scratch: cleanup pass over {} failed: {e}", dir.display());
            0
        }
    };

    if let Err(e) = scratch.shutdown() {
        warn!(
            "scratch: shutdown after sweep of {} failed: {e}",
            dir.display()
        );
    }
    recovered + cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "doom-engine-scratch-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup_dir(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn scratch_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn atomic_write_roundtrip() {
        let dir = test_dir("roundtrip");
        let target = dir.join("doom.cfg");
        atomic_write(&target, b"mouse_sensitivity\t\t9\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"mouse_sensitivity\t\t9\n");
        // the staged file must be renamed, not copied — nothing left over
        let leftovers = scratch_files(&dir.join(SCRATCH_DIR_NAME));
        assert!(
            leftovers.is_empty(),
            "unexpected scratch debris: {leftovers:?}"
        );
        cleanup_dir(&dir);
    }

    #[test]
    fn atomic_write_overwrites_existing_target() {
        let dir = test_dir("overwrite");
        let target = dir.join("doom.cfg");
        fs::write(&target, b"old contents").unwrap();
        atomic_write(&target, b"new contents").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new contents");
        cleanup_dir(&dir);
    }

    #[test]
    fn atomic_write_to_directory_target_leaves_no_debris() {
        let dir = test_dir("fail");
        let target = dir.join("is-a-dir");
        fs::create_dir(&target).unwrap();

        let err = atomic_write(&target, b"payload").unwrap_err();
        assert!(err.to_string().contains(target.to_str().unwrap()));

        // the staged file must be released on the error path, so the
        // scratch root holds no leftover files at all
        let leftovers = scratch_files(&dir.join(SCRATCH_DIR_NAME));
        assert!(
            leftovers.is_empty(),
            "unexpected scratch debris: {leftovers:?}"
        );
        cleanup_dir(&dir);
    }

    #[test]
    fn atomic_write_reports_missing_parent() {
        let dir = test_dir("pathctx");
        let target = dir.join("missing-parent").join("doom.cfg");
        let err = atomic_write(&target, b"payload").unwrap_err();
        assert!(err.to_string().contains("missing-parent"));
        cleanup_dir(&dir);
    }

    #[test]
    fn sweep_stale_reclaims_crashed_instance() {
        let root = test_dir("sweep");
        let dead = root.join("instance-crashed");
        fs::create_dir_all(&dead).unwrap();
        fs::write(
            dead.join(".cachegirl.lock"),
            "version=2\npid=2147483646\ncreated=1763100000\ninstance=instance-crashed\n",
        )
        .unwrap();
        fs::write(dead.join("payload.bin"), b"leftover").unwrap();

        let removed = sweep_stale(&root);
        assert!(removed >= 1, "expected at least one reclaimed entry");
        assert!(!dead.exists(), "stale instance dir must be reclaimed");
        cleanup_dir(&root);
    }

    #[test]
    fn sweep_stale_on_fresh_root_is_a_quiet_noop() {
        let root = test_dir("sweep-fresh");
        assert_eq!(sweep_stale(&root), 0);
        cleanup_dir(&root);
    }

    #[test]
    fn create_temp_materializes_a_writable_file() {
        let root = test_dir("create-temp");
        let file = ScratchTemp::create_temp(&root, "init", "probe.bin").unwrap();
        assert!(file.path().exists());
        assert!(file.path().starts_with(root.canonicalize().unwrap()));
        fs::write(file.path(), b"probe").unwrap();
        assert_eq!(fs::read(file.path()).unwrap(), b"probe");
        cleanup_dir(&root);
    }
}

//! Session persistence with advisory file locking.
//!
//! The session file holds the account's JWT pair plus the resolved
//! PDS endpoint. It is a *capability*: `fulmar login` (run once, by a
//! human who knows the password) creates it, and every other command
//! only ever refreshes it. When the refresh chain dies, commands fail
//! with exit code 3 and a message saying to re-run `fulmar login` —
//! never a prompt.
//!
//! ## Why locking exists
//!
//! AT Protocol refresh tokens rotate on use: `refreshSession` returns
//! a new JWT pair and invalidates the old refresh token. Two
//! concurrent invocations that both read the same refresh token will
//! race — the second `refreshSession` call fails and, worse, a stale
//! write can clobber the fresh pair, severing the chain. Since the
//! agent's environment holds no password (and `createSession` is
//! entryway-limited to ~100/day), a severed chain means human
//! intervention. So: every read takes a shared lock, and the
//! refresh path takes an exclusive lock *up front* (advisory locks
//! have no atomic shared→exclusive upgrade), re-reads the file after
//! acquiring it, and only refreshes if no other process already did.
//!
//! ## Why a separate lock file
//!
//! Writes are atomic (temp file + rename), which replaces the inode
//! at `session.json`. A process blocked on a lock of the *old* inode
//! would eventually acquire a lock on a ghost file and read
//! pre-refresh tokens — exactly the race this module exists to
//! prevent. The lock therefore lives on `session.json.lock`, which is
//! never renamed or replaced, and the data file is re-opened fresh
//! after every lock acquisition.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use etcetera::BaseStrategy;
use serde::{Deserialize, Serialize};

use crate::identifiers::{Did, Handle};

/// Environment variable overriding the session file location.
/// Precedence: `--session` flag, then this, then
/// `$XDG_STATE_HOME/fulmar/session.json`.
pub const SESSION_ENV: &str = "FULMAR_SESSION";

const SESSION_FILE_VERSION: u32 = 1;

/// Errors from session-file operations.
///
/// [`SessionError::Missing`] maps to exit code 3 at the CLI boundary:
/// the session capability is absent and only a human with the
/// password can restore it.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(
        "no session file at {path} — run `fulmar login` (once, by someone with the password) to create it"
    )]
    Missing { path: PathBuf },
    #[error("session file {path} is corrupt ({detail}) — re-run `fulmar login` to replace it")]
    Corrupt { path: PathBuf, detail: String },
    #[error("session file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "cannot determine a home directory for the session file; set ${SESSION_ENV} or pass --session <path>"
    )]
    NoHome,
}

/// The persisted session: identity, resolved service endpoint, and
/// the live JWT pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    /// Format version for forward compatibility.
    pub version: u32,
    pub did: Did,
    pub handle: Handle,
    /// The account's actual PDS endpoint, resolved from the DID
    /// document at login (NOT necessarily `bsky.social` — custom
    /// PDSes resolve here too). All authed calls go to this host.
    pub pds_url: String,
    pub access_jwt: String,
    pub refresh_jwt: String,
    pub updated_at: DateTime<Utc>,
}

impl SessionFile {
    /// Build a version-stamped session, timestamped now.
    #[must_use]
    pub fn new(
        did: Did,
        handle: Handle,
        pds_url: String,
        access_jwt: String,
        refresh_jwt: String,
    ) -> Self {
        Self {
            version: SESSION_FILE_VERSION,
            did,
            handle,
            pds_url,
            access_jwt,
            refresh_jwt,
            updated_at: Utc::now(),
        }
    }
}

/// Handle to the session file location. Cheap to construct; every
/// operation opens fresh file descriptors so that concurrent
/// invocations (and concurrent tasks within one invocation) contend
/// on the advisory lock correctly.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl SessionStore {
    /// Store at an explicit path.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let lock_path = lock_path_for(&path);
        Self { path, lock_path }
    }

    /// Resolve the session path: explicit flag beats `$FULMAR_SESSION`
    /// beats the XDG default (`~/.local/state/fulmar/session.json`).
    ///
    /// # Errors
    ///
    /// [`SessionError::NoHome`] when no explicit path is given and the
    /// platform home directory cannot be determined.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self, SessionError> {
        let env = std::env::var_os(SESSION_ENV).map(PathBuf::from);
        let path = resolve_path(explicit, env)?;
        Ok(Self::at(path))
    }

    /// The session file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the session under a shared lock.
    ///
    /// # Errors
    ///
    /// [`SessionError::Missing`] when no session file exists,
    /// [`SessionError::Corrupt`] when it doesn't parse, or
    /// [`SessionError::Io`] on filesystem failures.
    pub fn load(&self) -> Result<SessionFile, SessionError> {
        let lock = self.open_lock()?;
        lock.lock_shared().map_err(|source| self.io_err(source))?;
        self.read_data()
    }

    /// Write the session under an exclusive lock (used by `login`,
    /// which starts a brand-new refresh chain).
    ///
    /// # Errors
    ///
    /// [`SessionError::Io`] on filesystem failures.
    pub fn save(&self, session: &SessionFile) -> Result<(), SessionError> {
        let lock = self.open_lock()?;
        lock.lock().map_err(|source| self.io_err(source))?;
        self.write_data(session)
    }

    /// Take the exclusive lock for a read-refresh-write cycle. The
    /// caller MUST re-read via [`SessionGuard::read`] after acquiring
    /// and compare tokens: if another process already refreshed while
    /// we waited, adopt its tokens instead of spending the (now dead)
    /// refresh token we saw earlier.
    ///
    /// # Errors
    ///
    /// [`SessionError::Io`] when the lock file cannot be opened or
    /// locked.
    pub fn exclusive(&self) -> Result<SessionGuard, SessionError> {
        let lock = self.open_lock()?;
        lock.lock().map_err(|source| self.io_err(source))?;
        Ok(SessionGuard {
            store: self.clone(),
            lock,
        })
    }

    /// Remove the session file and its lock file.
    ///
    /// # Errors
    ///
    /// [`SessionError::Io`] on filesystem failures other than the
    /// files already being absent.
    pub fn delete(&self) -> Result<(), SessionError> {
        for path in [&self.path, &self.lock_path] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(SessionError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        Ok(())
    }

    fn open_lock(&self) -> Result<File, SessionError> {
        ensure_private_dir(self.lock_path.parent()).map_err(|source| SessionError::Io {
            path: self.lock_path.clone(),
            source,
        })?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)
            .map_err(|source| SessionError::Io {
                path: self.lock_path.clone(),
                source,
            })
    }

    fn read_data(&self) -> Result<SessionFile, SessionError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionError::Missing {
                    path: self.path.clone(),
                });
            }
            Err(source) => {
                return Err(SessionError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        serde_json::from_slice(&bytes).map_err(|e| SessionError::Corrupt {
            path: self.path.clone(),
            detail: e.to_string(),
        })
    }

    fn write_data(&self, session: &SessionFile) -> Result<(), SessionError> {
        let Some(dir) = self.path.parent() else {
            return Err(SessionError::Io {
                path: self.path.clone(),
                source: std::io::Error::other("session path has no parent directory"),
            });
        };
        let map_io = |source: std::io::Error| SessionError::Io {
            path: self.path.clone(),
            source,
        };
        let json = serde_json::to_vec_pretty(session).map_err(|e| SessionError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(e),
        })?;
        let tmp = tempfile::NamedTempFile::new_in(dir).map_err(map_io)?;
        fs::write(tmp.path(), json).map_err(map_io)?;
        restrict_permissions(tmp.path()).map_err(map_io)?;
        tmp.persist(&self.path).map_err(|e| map_io(e.error))?;
        Ok(())
    }

    fn io_err(&self, source: std::io::Error) -> SessionError {
        SessionError::Io {
            path: self.lock_path.clone(),
            source,
        }
    }
}

/// Exclusive hold on the session lock file. The advisory lock is
/// released when this guard drops (closing the lock fd). Owns a
/// clone of the store (two `PathBuf`s) so it can cross thread
/// boundaries — the refresh path acquires it inside
/// `spawn_blocking`.
#[derive(Debug)]
pub struct SessionGuard {
    store: SessionStore,
    #[allow(dead_code)]
    lock: File,
}

impl SessionGuard {
    /// Re-read the session file fresh (never a cached fd — see the
    /// module docs on rename-vs-inode).
    ///
    /// # Errors
    ///
    /// Same as [`SessionStore::load`].
    pub fn read(&self) -> Result<SessionFile, SessionError> {
        self.store.read_data()
    }

    /// Atomically replace the session file while holding the lock.
    ///
    /// # Errors
    ///
    /// [`SessionError::Io`] on filesystem failures.
    pub fn write(&self, session: &SessionFile) -> Result<(), SessionError> {
        self.store.write_data(session)
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("session.json"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".lock");
    path.with_file_name(name)
}

fn resolve_path(explicit: Option<PathBuf>, env: Option<PathBuf>) -> Result<PathBuf, SessionError> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(path) = env {
        return Ok(path);
    }
    let Ok(strategy) = etcetera::choose_base_strategy() else {
        return Err(SessionError::NoHome);
    };
    let Some(state_dir) = strategy.state_dir() else {
        return Err(SessionError::NoHome);
    };
    Ok(state_dir.join("fulmar").join("session.json"))
}

fn ensure_private_dir(dir: Option<&Path>) -> std::io::Result<()> {
    let Some(dir) = dir else {
        return Ok(());
    };
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    restrict_dir_permissions(dir)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifiers::{Did, Handle};

    fn sample(counter: u64) -> SessionFile {
        SessionFile::new(
            Did::from_trusted("did:plc:testtesttest"),
            Handle::from_trusted("test.example.com"),
            "https://pds.example.com".to_string(),
            format!("access-{counter}"),
            format!("{counter}"),
        )
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::at(dir.path().join("session.json"));
        store.save(&sample(1)).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(loaded.refresh_jwt, "1");
        assert_eq!(loaded.pds_url, "https://pds.example.com");
        assert_eq!(loaded.version, 1);
    }

    #[test]
    fn load_missing_names_login_in_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::at(dir.path().join("session.json"));
        let err = store.load().expect_err("must be missing");
        let msg = err.to_string();
        assert!(msg.contains("fulmar login"), "got: {msg}");
    }

    #[test]
    fn load_corrupt_names_the_problem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.json");
        fs::write(&path, b"not json{").expect("write");
        let store = SessionStore::at(path);
        let err = store.load().expect_err("must be corrupt");
        let SessionError::Corrupt { .. } = err else {
            panic!("expected Corrupt, got {err:?}");
        };
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::at(dir.path().join("session.json"));
        store.save(&sample(1)).expect("save");
        store.delete().expect("delete");
        store.delete().expect("second delete is fine");
        let err = store.load().expect_err("gone");
        let SessionError::Missing { .. } = err else {
            panic!("expected Missing, got {err:?}");
        };
    }

    #[test]
    fn explicit_path_beats_env_beats_default() {
        let explicit = PathBuf::from("/tmp/explicit.json");
        let env = PathBuf::from("/tmp/env.json");
        assert_eq!(
            resolve_path(Some(explicit.clone()), Some(env.clone())).expect("resolve"),
            explicit
        );
        assert_eq!(resolve_path(None, Some(env.clone())).expect("resolve"), env);
        let default = resolve_path(None, None).expect("resolve");
        assert!(default.ends_with("fulmar/session.json"), "got {default:?}");
    }

    #[cfg(unix)]
    #[test]
    fn session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::at(dir.path().join("session.json"));
        store.save(&sample(1)).expect("save");
        let mode = fs::metadata(store.path())
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "session file must be 0600");
    }

    /// The load-bearing test: N contenders with *separate file
    /// descriptors* (flock contention is per open-file-description,
    /// so separate handles in threads race exactly like separate
    /// processes) each perform read-modify-write cycles through the
    /// exclusive guard. Every increment must survive: a lost update
    /// here is the token-rotation race that severs refresh chains.
    #[test]
    fn concurrent_read_modify_write_loses_no_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.json");
        SessionStore::at(path.clone())
            .save(&sample(0))
            .expect("seed");

        let threads = 8;
        let iterations = 25;
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = SessionStore::at(path);
                    for _ in 0..iterations {
                        let guard = store.exclusive().expect("lock");
                        let current = guard.read().expect("read under lock");
                        let counter: u64 = current.refresh_jwt.parse().expect("counter token");
                        guard.write(&sample(counter + 1)).expect("write under lock");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("thread");
        }

        let final_state = SessionStore::at(path).load().expect("final load");
        assert_eq!(
            final_state.refresh_jwt,
            (threads * iterations).to_string(),
            "every read-modify-write must be serialized by the lock"
        );
    }
}

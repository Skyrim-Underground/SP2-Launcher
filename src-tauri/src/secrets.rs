// SPDX-License-Identifier: AGPL-3.0-or-later
//! Secrets at rest: DPAPI for the current Windows user, protected files written by rename, and the per-server sign-in files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::slice;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, CRYPT_INTEGER_BLOB,
    CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

use crate::config::write_atomic;

pub const SESSIONS_DIR: &str = "sessions";
pub const SESSION_SCHEMA_VERSION: u32 = 1;
const SESSION_SUFFIX: &str = ".bin";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the data is too large to protect")]
    TooLarge,
    #[error("the data could not be protected: {0}")]
    Protect(windows::core::Error),
    #[error(
        "the data could not be read back: it is damaged or was protected by another Windows user"
    )]
    Unprotect,
    #[error("random bytes could not be drawn from the system: {0}")]
    Random(windows::core::Error),
    #[error("the stored sign-in is not valid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
}

/// Bytes from the system's random number generator, fit for secrets.
pub fn random_bytes(len: usize) -> Result<Vec<u8>, SecretError> {
    let mut bytes = vec![0u8; len];
    // SAFETY: the buffer is a live local slice for the whole call.
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .map_err(SecretError::Random)?;
    Ok(bytes)
}

fn io_at(path: &Path, source: io::Error) -> SecretError {
    SecretError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    }
}

fn blob_of(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, SecretError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| SecretError::TooLarge)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// Copies the bytes the system allocated for a blob and frees them; the blob must come from one of the two calls below and not have been freed.
/// A blob without a buffer holds nothing, and there is nothing to free.
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: the system allocated cbData bytes at pbData, which are read once and freed once, here.
    unsafe {
        let bytes = match blob.cbData {
            0 => Vec::new(),
            length => slice::from_raw_parts(blob.pbData, length as usize).to_vec(),
        };
        LocalFree(Some(HLOCAL(blob.pbData.cast())));
        bytes
    }
}

/// Wraps the bytes so that only the same Windows user can unwrap them; no prompt is ever shown.
pub fn protect(plain: &[u8]) -> Result<Vec<u8>, SecretError> {
    let input = blob_of(plain)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the input blob points into `plain`, which outlives the call, and the output blob is taken right after it.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(SecretError::Protect)?;
        Ok(take_blob(output))
    }
}

/// Unwraps bytes that `protect` produced for this Windows user.
pub fn unprotect(protected: &[u8]) -> Result<Vec<u8>, SecretError> {
    let input = blob_of(protected)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the input blob points into `protected`, which outlives the call, and the output blob is taken right after it.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|_| SecretError::Unprotect)?;
        Ok(take_blob(output))
    }
}

/// Writes the protected form of the bytes through a temp file and a rename.
pub fn write_protected(path: &Path, plain: &[u8]) -> Result<(), SecretError> {
    let protected = protect(plain)?;
    write_atomic(path, &protected).map_err(|error| io_at(path, error))
}

/// The unwrapped content of a protected file, or None when there is no such file.
pub fn read_protected(path: &Path) -> Result<Option<Vec<u8>>, SecretError> {
    match fs::read(path) {
        Ok(protected) => unprotect(&protected).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_at(path, error)),
    }
}

/// A server's sign-in as its backend issued it; `expires_at` is milliseconds since the Unix epoch, as the backend sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Session {
    schema_version: u32,
    pub session: String,
    pub expires_at: u64,
    pub display_name: String,
}

impl Session {
    pub fn new(session: String, expires_at: u64, display_name: String) -> Self {
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            session,
            expires_at,
            display_name,
        }
    }
}

/// One protected file per server, named by the server's id, so a session can only ever be read for the server it came from.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(SESSIONS_DIR),
        }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}{SESSION_SUFFIX}"))
    }

    /// The stored sign-in, or None when there is none; one that has run out or has another schema is removed.
    pub fn load(&self, id: &str, now_ms: u64) -> Result<Option<Session>, SecretError> {
        let Some(bytes) = read_protected(&self.path(id))? else {
            return Ok(None);
        };
        let session: Session = serde_json::from_slice(&bytes)?;
        if session.schema_version != SESSION_SCHEMA_VERSION || session.expires_at <= now_ms {
            self.delete(id)?;
            return Ok(None);
        }
        Ok(Some(session))
    }

    pub fn save(&self, id: &str, session: &Session) -> Result<(), SecretError> {
        write_protected(&self.path(id), &serde_json::to_vec(session)?)
    }

    /// Removes the stored sign-in; the answer says whether there was one.
    pub fn delete(&self, id: &str) -> Result<bool, SecretError> {
        let path = self.path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_at(&path, error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "sugs_TESTtestTESTtestTESTtestTESTtestTESTtes";

    #[test]
    fn random_bytes_come_in_the_asked_length_and_differ() {
        let first = random_bytes(32).unwrap();
        let second = random_bytes(32).unwrap();
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
        assert!(random_bytes(0).unwrap().is_empty());
    }

    #[test]
    fn sessions_are_stored_per_server_under_dpapi_and_run_out() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path());
        assert_eq!(store.load("a.example.org", 1_000).unwrap(), None);
        let session = Session::new(SESSION.to_string(), 2_000, "Tester".to_string());
        store.save("a.example.org", &session).unwrap();
        assert_eq!(
            store.path("a.example.org").file_name().unwrap(),
            "a.example.org.bin"
        );
        let stored = fs::read(store.path("a.example.org")).unwrap();
        assert!(
            !stored
                .windows(SESSION.len())
                .any(|w| w == SESSION.as_bytes())
        );
        assert_eq!(
            store.load("a.example.org", 1_999).unwrap(),
            Some(session.clone())
        );
        assert_eq!(store.load("b.example.org", 1_999).unwrap(), None);
        // The moment the backend named has passed, so the file is useless and goes.
        assert_eq!(store.load("a.example.org", 2_000).unwrap(), None);
        assert!(!store.path("a.example.org").exists());
        store.save("a.example.org", &session).unwrap();
        assert!(store.delete("a.example.org").unwrap());
        assert!(!store.delete("a.example.org").unwrap());
        assert_eq!(store.load("a.example.org", 0).unwrap(), None);
    }

    #[test]
    fn a_session_file_of_another_shape_or_left_unprotected_is_not_a_session() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path());
        let newer =
            br#"{"schemaVersion":2,"session":"sugs_x","expiresAt":9,"displayName":"T","extra":1}"#;
        write_protected(&store.path("a"), newer).unwrap();
        assert!(matches!(store.load("a", 0), Err(SecretError::Json(_))));
        let other_version =
            br#"{"schemaVersion":2,"session":"sugs_x","expiresAt":9,"displayName":"T"}"#;
        write_protected(&store.path("a"), other_version).unwrap();
        assert_eq!(store.load("a", 0).unwrap(), None);
        assert!(!store.path("a").exists());
        fs::write(store.path("a"), b"plain, not protected").unwrap();
        assert!(matches!(store.load("a", 0), Err(SecretError::Unprotect)));
    }

    #[test]
    fn dpapi_round_trips_and_refuses_damaged_data() {
        let plain = b"{\"playToken\":\"sugp_not_a_real_token\"}";
        let protected = protect(plain).unwrap();
        assert_ne!(protected, plain);
        assert!(!protected.windows(plain.len()).any(|w| w == plain));
        assert_eq!(unprotect(&protected).unwrap(), plain);
        let mut damaged = protected.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 0xff;
        assert!(matches!(unprotect(&damaged), Err(SecretError::Unprotect)));
        assert!(matches!(
            unprotect(b"not a blob"),
            Err(SecretError::Unprotect)
        ));
        assert_eq!(unprotect(&protect(b"").unwrap()).unwrap(), b"");
    }

    #[test]
    fn protected_files_are_written_whole_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("secret.bin");
        assert_eq!(read_protected(&path).unwrap(), None);
        write_protected(&path, b"one").unwrap();
        write_protected(&path, b"two").unwrap();
        assert_eq!(read_protected(&path).unwrap(), Some(b"two".to_vec()));
        let stored = fs::read(&path).unwrap();
        assert!(!stored.windows(3).any(|w| w == b"two"));
        let left: Vec<_> = fs::read_dir(dir.path().join("nested"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["secret.bin"]);
        fs::write(&path, b"plain, not protected").unwrap();
        assert!(matches!(read_protected(&path), Err(SecretError::Unprotect)));
    }
}

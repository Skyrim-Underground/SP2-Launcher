// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server list: entries from the signed directory merged with servers the player added by URL.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use minisign_verify::{PublicKey, Signature};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::api::{ApiClient, ApiError};
use crate::config::{StoreFile, read_store, write_atomic};
use crate::manifest::is_reserved;

pub const SCHEMA_VERSION: u32 = 1;
pub const FILE_NAME: &str = "servers.json";
const DIRECTORY_CACHE: &str = "directory.json";
const DIRECTORY_SIGNATURE_CACHE: &str = "directory.json.minisig";
const DIRECTORY_MAX_BYTES: usize = 256 * 1024;
const SIGNATURE_MAX_BYTES: usize = 4 * 1024;

/// Shown before a server that is not on the directory is added.
pub const UNLISTED_WARNING: &str = "This server is not on the directory. Playing on it installs files, including native code, that its owner chooses. Add it only if you trust the owner.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    pub backend_url: Url,
    pub listed: bool,
    pub added_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServersFile {
    schema_version: u32,
    servers: Vec<ServerEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Directory {
    #[serde(rename = "$comment", default)]
    _comment: IgnoredAny,
    pub schema_version: u32,
    pub servers: Vec<DirectoryEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryEntry {
    pub name: String,
    pub backend_url: Url,
}

/// A directory whose signature checked out, with the bytes to keep as the last good copy.
pub struct VerifiedDirectory {
    pub directory: Directory,
    bytes: Vec<u8>,
    signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DirectoryReport {
    Fresh,
    Cached { reason: String },
    Unavailable { reason: String },
    Unconfigured,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("servers.json: {0}")]
    Io(#[from] io::Error),
    #[error("servers.json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OriginError {
    #[error("enter a URL that starts with https://")]
    NotHttps,
    #[error("the URL is not valid")]
    Invalid,
    #[error("the URL must not carry a user name or password")]
    Credentials,
    #[error("the URL must be an origin only, without a path")]
    Path,
    #[error("the URL must be an origin only, without a query")]
    Query,
    #[error("the URL must be an origin only, without a fragment")]
    Fragment,
    #[error("the host name must not end with a dot")]
    Host,
    #[error("the host name begins with a name Windows reserves for devices")]
    ReservedHost,
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("{0}")]
    Fetch(#[from] ApiError),
    #[error("the signature of the directory is not valid")]
    Signature,
    #[error("the directory is not valid: {0}")]
    Parse(String),
    #[error("the directory has schema version {0}, this launcher knows {SCHEMA_VERSION}")]
    Schema(u32),
}

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error("this server is already in the list")]
    AlreadyAdded,
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, thiserror::Error)]
pub enum RemoveError {
    #[error("unknown server")]
    NotFound,
    #[error("a server from the directory cannot be removed")]
    Listed,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Accepts an https origin and nothing more, so an entry never carries a path, a query or credentials.
pub fn validate_origin(input: &str) -> Result<Url, OriginError> {
    let input = input.trim();
    if !input
        .get(..8)
        .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
    {
        return Err(OriginError::NotHttps);
    }
    let url = Url::parse(input).map_err(|_| OriginError::Invalid)?;
    let host = url.host_str().ok_or(OriginError::Invalid)?;
    // The host names the server's folders and files; Windows drops a trailing dot, "." or ".." would leave the folder, and a device name is never a file.
    if host.ends_with('.') {
        return Err(OriginError::Host);
    }
    if is_reserved(host) {
        return Err(OriginError::ReservedHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OriginError::Credentials);
    }
    if url.path() != "/" {
        return Err(OriginError::Path);
    }
    if url.query().is_some() {
        return Err(OriginError::Query);
    }
    if url.fragment().is_some() {
        return Err(OriginError::Fragment);
    }
    Ok(url)
}

/// The folder-safe name of a backend: its host, plus the port when it is not the default one.
pub fn server_id(origin: &Url) -> String {
    let mut id: String = origin
        .host_str()
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if let Some(port) = origin.port() {
        id.push('-');
        id.push_str(&port.to_string());
    }
    id
}

pub struct ServerStore {
    path: PathBuf,
    cache_dir: PathBuf,
    entries: Vec<ServerEntry>,
    set_aside: Option<String>,
}

impl ServerStore {
    pub fn load(config_dir: &Path, cache_dir: &Path) -> Result<Self, StoreError> {
        let path = config_dir.join(FILE_NAME);
        let (entries, set_aside) = match read_store::<ServersFile>(&path, SCHEMA_VERSION)? {
            StoreFile::Read(file) => (file.servers, None),
            StoreFile::Missing => (Vec::new(), None),
            StoreFile::SetAside(reason) => (Vec::new(), Some(reason)),
        };
        Ok(Self {
            path,
            cache_dir: cache_dir.to_path_buf(),
            entries,
            set_aside,
        })
    }

    pub fn entries(&self) -> &[ServerEntry] {
        &self.entries
    }

    /// Why the file on disk was set aside at load, given out once so the UI shows it once.
    pub fn take_set_aside(&mut self) -> Option<String> {
        self.set_aside.take()
    }

    pub fn get(&self, id: &str) -> Option<&ServerEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn save(&self) -> Result<(), StoreError> {
        let file = ServersFile {
            schema_version: SCHEMA_VERSION,
            servers: self.entries.clone(),
        };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)?;
        Ok(())
    }

    /// Listed entries follow the directory exactly; manual entries stay unless the directory now lists them.
    /// The file is rewritten only when the list changed. The answer names the listed servers that left the list.
    pub fn apply_directory(
        &mut self,
        directory: &Directory,
        now: u64,
    ) -> Result<Vec<String>, StoreError> {
        let mut merged: Vec<ServerEntry> = directory
            .servers
            .iter()
            .map(|listed| {
                let id = server_id(&listed.backend_url);
                let added_at = self.get(&id).map_or(now, |known| known.added_at);
                ServerEntry {
                    id,
                    name: listed.name.clone(),
                    backend_url: listed.backend_url.clone(),
                    listed: true,
                    added_at,
                }
            })
            .collect();
        for entry in &self.entries {
            if !entry.listed && !merged.iter().any(|known| known.id == entry.id) {
                merged.push(entry.clone());
            }
        }
        let delisted: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| entry.listed && !merged.iter().any(|known| known.id == entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        if merged == self.entries {
            return Ok(delisted);
        }
        self.entries = merged;
        self.save()?;
        Ok(delisted)
    }

    pub fn add_manual(
        &mut self,
        origin: Url,
        name: String,
        now: u64,
    ) -> Result<ServerEntry, AddError> {
        let id = server_id(&origin);
        if self.get(&id).is_some() {
            return Err(AddError::AlreadyAdded);
        }
        let entry = ServerEntry {
            id,
            name,
            backend_url: origin,
            listed: false,
            added_at: now,
        };
        self.entries.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    pub fn remove(&mut self, id: &str) -> Result<(), RemoveError> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or(RemoveError::NotFound)?;
        if self.entries[index].listed {
            return Err(RemoveError::Listed);
        }
        self.entries.remove(index);
        self.save()?;
        Ok(())
    }

    /// Applies a fetched directory, or falls back to the last good copy when the fetch or its signature failed.
    /// Next to the report, the answer names the listed servers that left the list, so what was kept for them can go.
    pub fn take_directory(
        &mut self,
        key: &PublicKey,
        fetched: Result<VerifiedDirectory, DirectoryError>,
        now: u64,
    ) -> Result<(DirectoryReport, Vec<String>), StoreError> {
        match fetched {
            Ok(verified) => {
                let delisted = self.apply_directory(&verified.directory, now)?;
                write_atomic(&self.cache_dir.join(DIRECTORY_CACHE), &verified.bytes)?;
                write_atomic(
                    &self.cache_dir.join(DIRECTORY_SIGNATURE_CACHE),
                    &verified.signature,
                )?;
                Ok((DirectoryReport::Fresh, delisted))
            }
            Err(error) => {
                let reason = error.to_string();
                let cached = self
                    .read_cache()
                    .and_then(|(bytes, signature)| verify_directory(key, &bytes, &signature).ok());
                match cached {
                    Some(directory) => {
                        let delisted = self.apply_directory(&directory, now)?;
                        Ok((DirectoryReport::Cached { reason }, delisted))
                    }
                    None => Ok((DirectoryReport::Unavailable { reason }, Vec::new())),
                }
            }
        }
    }

    fn read_cache(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let bytes = fs::read(self.cache_dir.join(DIRECTORY_CACHE)).ok()?;
        let signature = fs::read(self.cache_dir.join(DIRECTORY_SIGNATURE_CACHE)).ok()?;
        Some((bytes, signature))
    }
}

/// Checks the signature over the exact bytes served, then the shape, then every entry's origin.
pub fn verify_directory(
    key: &PublicKey,
    bytes: &[u8],
    signature: &[u8],
) -> Result<Directory, DirectoryError> {
    let signature_text = std::str::from_utf8(signature).map_err(|_| DirectoryError::Signature)?;
    let signature = Signature::decode(signature_text).map_err(|_| DirectoryError::Signature)?;
    key.verify(bytes, &signature, false)
        .map_err(|_| DirectoryError::Signature)?;
    let directory: Directory =
        serde_json::from_slice(bytes).map_err(|error| DirectoryError::Parse(error.to_string()))?;
    if directory.schema_version != SCHEMA_VERSION {
        return Err(DirectoryError::Schema(directory.schema_version));
    }
    for (index, entry) in directory.servers.iter().enumerate() {
        validate_origin(entry.backend_url.as_str()).map_err(|error| {
            DirectoryError::Parse(format!(
                "entry {} has an unusable backend URL: {error}",
                index + 1
            ))
        })?;
    }
    Ok(directory)
}

/// Fetches the directory and its detached signature, which sits next to it under the same name plus `.minisig`.
pub async fn fetch_directory(
    client: &ApiClient,
    url: &Url,
    key: &PublicKey,
) -> Result<VerifiedDirectory, DirectoryError> {
    let mut signature_url = url.clone();
    signature_url.set_path(&format!("{}.minisig", url.path()));
    let bytes = client.get_bytes(url, DIRECTORY_MAX_BYTES).await?;
    let signature = client
        .get_bytes(&signature_url, SIGNATURE_MAX_BYTES)
        .await?;
    let directory = verify_directory(key, &bytes, &signature)?;
    Ok(VerifiedDirectory {
        directory,
        bytes,
        signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::CannedServer;

    const FIXTURE: &str = include_str!("../../test/fixtures/directory.json");
    const NOW: u64 = 1_760_000_000;

    fn test_key() -> (minisign::KeyPair, PublicKey) {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let public = PublicKey::decode(&pair.pk.to_box().unwrap().into_string()).unwrap();
        (pair, public)
    }

    fn sign(pair: &minisign::KeyPair, bytes: &[u8]) -> Vec<u8> {
        minisign::sign(Some(&pair.pk), &pair.sk, bytes, Some("directory"), None)
            .unwrap()
            .into_string()
            .into_bytes()
    }

    fn store(root: &Path) -> ServerStore {
        ServerStore::load(&root.join("config"), &root.join("cache")).unwrap()
    }

    fn origin(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn only_a_bare_https_origin_is_accepted() {
        assert_eq!(
            validate_origin(" https://api.example.org ")
                .unwrap()
                .as_str(),
            "https://api.example.org/"
        );
        assert!(validate_origin("https://api.example.org/").is_ok());
        assert!(validate_origin("HTTPS://api.example.org:8443").is_ok());
        assert_eq!(
            validate_origin("http://api.example.org"),
            Err(OriginError::NotHttps)
        );
        assert_eq!(
            validate_origin("api.example.org"),
            Err(OriginError::NotHttps)
        );
        assert_eq!(
            validate_origin("https://api.example.org/api"),
            Err(OriginError::Path)
        );
        assert_eq!(
            validate_origin("https://api.example.org/?x=1"),
            Err(OriginError::Query)
        );
        assert_eq!(
            validate_origin("https://api.example.org/#top"),
            Err(OriginError::Fragment)
        );
        assert_eq!(
            validate_origin("https://user:pw@api.example.org"),
            Err(OriginError::Credentials)
        );
        assert_eq!(validate_origin("https://"), Err(OriginError::Invalid));
        for dots in [
            "https://..",
            "https://.",
            "https://%2e%2e",
            "https://...:8443",
            "https://api.example.org.",
        ] {
            assert_eq!(validate_origin(dots), Err(OriginError::Host), "{dots}");
        }
        for device in [
            "https://nul.example.org",
            "https://com1.example.org",
            "https://CON:8443",
        ] {
            assert_eq!(
                validate_origin(device),
                Err(OriginError::ReservedHost),
                "{device}"
            );
        }
        assert!(validate_origin("https://console.example.org").is_ok());
    }

    #[test]
    fn ids_come_from_the_origin() {
        assert_eq!(
            server_id(&origin("https://Api.Example.org")),
            "api.example.org"
        );
        assert_eq!(
            server_id(&origin("https://api.example.org:8443")),
            "api.example.org-8443"
        );
        assert_eq!(
            server_id(&origin("https://api.example.org:443")),
            "api.example.org"
        );
        assert_eq!(
            server_id(&origin("https://[::1]:8443")),
            "_::1_-8443".replace(':', "_")
        );
    }

    #[test]
    fn manual_entries_persist_and_can_be_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut servers = store(root.path());
        assert!(servers.entries().is_empty());
        let entry = servers
            .add_manual(origin("https://api.example.org"), "Example".into(), NOW)
            .unwrap();
        assert_eq!(entry.id, "api.example.org");
        assert!(!entry.listed);
        assert!(matches!(
            servers.add_manual(origin("https://api.example.org/"), "Again".into(), NOW),
            Err(AddError::AlreadyAdded)
        ));
        let reloaded = store(root.path());
        assert_eq!(reloaded.entries(), servers.entries());
        assert!(matches!(
            servers.remove("missing"),
            Err(RemoveError::NotFound)
        ));
        servers.remove("api.example.org").unwrap();
        assert!(store(root.path()).entries().is_empty());
    }

    #[test]
    fn a_foreign_schema_is_set_aside_and_reported_once() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        fs::create_dir_all(&config).unwrap();
        let newer = br#"{"schemaVersion":7,"servers":[]}"#;
        fs::write(config.join(FILE_NAME), newer).unwrap();
        let mut servers = store(root.path());
        assert!(servers.entries().is_empty());
        let reason = servers.take_set_aside().unwrap();
        assert!(reason.contains("schema version 7"), "{reason}");
        assert!(reason.contains("servers.json.bad"), "{reason}");
        assert_eq!(servers.take_set_aside(), None);
        assert!(!config.join(FILE_NAME).exists());
        assert_eq!(fs::read(config.join("servers.json.bad")).unwrap(), newer);
        servers
            .add_manual(origin("https://api.example.org"), "Example".into(), NOW)
            .unwrap();
        let mut reloaded = store(root.path());
        assert_eq!(reloaded.entries().len(), 1);
        assert_eq!(reloaded.take_set_aside(), None);
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join(FILE_NAME), b"not json").unwrap();
        let mut servers = store(root.path());
        assert!(servers.entries().is_empty());
        assert!(servers.take_set_aside().is_some());
        assert!(config.join("servers.json.bad").exists());
    }

    #[test]
    fn a_signed_directory_verifies_and_a_tampered_one_does_not() {
        let (pair, public) = test_key();
        let signature = sign(&pair, FIXTURE.as_bytes());
        let directory = verify_directory(&public, FIXTURE.as_bytes(), &signature).unwrap();
        assert_eq!(directory.servers.len(), 2);
        assert_eq!(directory.servers[0].name, "Fixture One");
        let tampered = FIXTURE.replace("Fixture One", "Fixture Evil");
        assert!(matches!(
            verify_directory(&public, tampered.as_bytes(), &signature),
            Err(DirectoryError::Signature)
        ));
        let (_, other_public) = test_key();
        assert!(matches!(
            verify_directory(&other_public, FIXTURE.as_bytes(), &signature),
            Err(DirectoryError::Signature)
        ));
        assert!(matches!(
            verify_directory(&public, FIXTURE.as_bytes(), b"not a signature"),
            Err(DirectoryError::Signature)
        ));
    }

    #[test]
    fn a_signed_directory_still_needs_the_right_shape() {
        let (pair, public) = test_key();
        let newer = r#"{"schemaVersion":2,"servers":[]}"#;
        assert!(matches!(
            verify_directory(&public, newer.as_bytes(), &sign(&pair, newer.as_bytes())),
            Err(DirectoryError::Schema(2))
        ));
        let http = r#"{"schemaVersion":1,"servers":[{"name":"Plain","backendUrl":"http://plain.example.org"}]}"#;
        assert!(matches!(
            verify_directory(&public, http.as_bytes(), &sign(&pair, http.as_bytes())),
            Err(DirectoryError::Parse(_))
        ));
        let extra = r#"{"schemaVersion":1,"servers":[{"name":"X","backendUrl":"https://x.example.org","featured":true}]}"#;
        assert!(matches!(
            verify_directory(&public, extra.as_bytes(), &sign(&pair, extra.as_bytes())),
            Err(DirectoryError::Parse(_))
        ));
    }

    #[test]
    fn the_directory_merges_with_manual_entries() {
        let (pair, public) = test_key();
        let root = tempfile::tempdir().unwrap();
        let mut servers = store(root.path());
        servers
            .add_manual(origin("https://two.example.org:8443"), "My Two".into(), 5)
            .unwrap();
        servers
            .add_manual(origin("https://mine.example.org"), "Mine".into(), 6)
            .unwrap();
        let fetched = Ok(VerifiedDirectory {
            directory: verify_directory(
                &public,
                FIXTURE.as_bytes(),
                &sign(&pair, FIXTURE.as_bytes()),
            )
            .unwrap(),
            bytes: FIXTURE.as_bytes().to_vec(),
            signature: sign(&pair, FIXTURE.as_bytes()),
        });
        let (report, delisted) = servers.take_directory(&public, fetched, NOW).unwrap();
        assert_eq!(report, DirectoryReport::Fresh);
        assert!(delisted.is_empty(), "{delisted:?}");
        let ids: Vec<&str> = servers.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "one.example.org",
                "two.example.org-8443",
                "mine.example.org"
            ]
        );
        let two = servers.get("two.example.org-8443").unwrap();
        assert!(two.listed);
        assert_eq!(two.name, "Fixture Two");
        assert_eq!(two.added_at, 5);
        assert!(!servers.get("mine.example.org").unwrap().listed);
        assert!(root.path().join("cache").join(DIRECTORY_CACHE).exists());
        assert!(
            root.path()
                .join("cache")
                .join(DIRECTORY_SIGNATURE_CACHE)
                .exists()
        );
        assert!(matches!(
            servers.remove("one.example.org"),
            Err(RemoveError::Listed)
        ));
        let same = Ok(VerifiedDirectory {
            directory: verify_directory(
                &public,
                FIXTURE.as_bytes(),
                &sign(&pair, FIXTURE.as_bytes()),
            )
            .unwrap(),
            bytes: FIXTURE.as_bytes().to_vec(),
            signature: sign(&pair, FIXTURE.as_bytes()),
        });
        fs::remove_file(root.path().join("config").join(FILE_NAME)).unwrap();
        let (_, delisted) = servers.take_directory(&public, same, NOW).unwrap();
        assert!(delisted.is_empty(), "{delisted:?}");
        assert!(
            !root.path().join("config").join(FILE_NAME).exists(),
            "an unchanged list must not be written again"
        );
        let smaller = r#"{"schemaVersion":1,"servers":[{"name":"Fixture One","backendUrl":"https://one.example.org"}]}"#;
        let fetched = Ok(VerifiedDirectory {
            directory: verify_directory(
                &public,
                smaller.as_bytes(),
                &sign(&pair, smaller.as_bytes()),
            )
            .unwrap(),
            bytes: smaller.as_bytes().to_vec(),
            signature: sign(&pair, smaller.as_bytes()),
        });
        let (_, delisted) = servers.take_directory(&public, fetched, NOW).unwrap();
        let ids: Vec<&str> = servers.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["one.example.org", "mine.example.org"]);
        // The listed server that left the directory is named, so its stored sign-in can go with the entry.
        assert_eq!(delisted, ["two.example.org-8443"]);
    }

    #[test]
    fn a_bad_signature_keeps_the_old_copy() {
        let (pair, public) = test_key();
        let root = tempfile::tempdir().unwrap();
        let mut servers = store(root.path());
        let signature = sign(&pair, FIXTURE.as_bytes());
        let fetched = Ok(VerifiedDirectory {
            directory: verify_directory(&public, FIXTURE.as_bytes(), &signature).unwrap(),
            bytes: FIXTURE.as_bytes().to_vec(),
            signature: signature.clone(),
        });
        servers.take_directory(&public, fetched, NOW).unwrap();
        let before = servers.entries().to_vec();
        let cached_bytes = fs::read(root.path().join("cache").join(DIRECTORY_CACHE)).unwrap();
        let (report, delisted) = servers
            .take_directory(&public, Err(DirectoryError::Signature), NOW)
            .unwrap();
        assert_eq!(
            report,
            DirectoryReport::Cached {
                reason: "the signature of the directory is not valid".into()
            }
        );
        assert!(delisted.is_empty(), "{delisted:?}");
        assert_eq!(servers.entries(), before);
        assert_eq!(
            fs::read(root.path().join("cache").join(DIRECTORY_CACHE)).unwrap(),
            cached_bytes
        );
        let fresh_root = tempfile::tempdir().unwrap();
        let mut fresh = store(fresh_root.path());
        let (report, _) = fresh
            .take_directory(&public, Err(DirectoryError::Signature), NOW)
            .unwrap();
        assert!(matches!(report, DirectoryReport::Unavailable { .. }));
        assert!(fresh.entries().is_empty());
    }

    #[tokio::test]
    async fn the_directory_is_fetched_with_its_detached_signature() {
        let (pair, public) = test_key();
        let client = ApiClient::new("sug-launcher-tests/0").unwrap();
        let site = CannedServer::start();
        let url = site.origin().join("/launcher/directory.json").unwrap();
        site.bytes("/launcher/directory.json", FIXTURE.as_bytes().to_vec());
        site.bytes(
            "/launcher/directory.json.minisig",
            sign(&pair, FIXTURE.as_bytes()),
        );
        let verified = fetch_directory(&client, &url, &public).await.unwrap();
        assert_eq!(verified.directory.servers.len(), 2);
        let (other, _) = test_key();
        site.bytes(
            "/launcher/directory.json.minisig",
            sign(&other, FIXTURE.as_bytes()),
        );
        assert!(matches!(
            fetch_directory(&client, &url, &public).await,
            Err(DirectoryError::Signature)
        ));
        site.json("/launcher/directory.json.minisig", 404, "{}");
        assert!(matches!(
            fetch_directory(&client, &url, &public).await,
            Err(DirectoryError::Fetch(ApiError::Status(404)))
        ));
    }
}

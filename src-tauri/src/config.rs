// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bundled launcher.json, the signing key from tauri.conf.json, the app folders and settings.json.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use minisign_verify::PublicKey;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use url::Url;

const LAUNCHER_JSON: &str = include_str!("../../launcher.json");
pub const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
const SET_ASIDE_SUFFIX: &str = ".bad";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("launcher.json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("launcher.json: {0} must be an https URL")]
    NotHttps(&'static str),
    #[error("tauri.conf.json: plugins.updater.pubkey is not a minisign public key")]
    Key,
}

/// The one file a fork edits for its own directory and website; branding stays in tauri.conf.json.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LauncherConfig {
    #[serde(rename = "$comment", default)]
    _comment: IgnoredAny,
    pub directory_url: Url,
    pub website: Url,
    pub rules_url: Url,
}

impl LauncherConfig {
    pub fn bundled() -> Result<Self, ConfigError> {
        Self::parse(LAUNCHER_JSON)
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = serde_json::from_str(text)?;
        let urls = [
            ("directoryUrl", &config.directory_url),
            ("website", &config.website),
            ("rulesUrl", &config.rules_url),
        ];
        for (name, url) in urls {
            if url.scheme() != "https" {
                return Err(ConfigError::NotHttps(name));
            }
        }
        Ok(config)
    }
}

/// The updater's config as far as the directory needs it; the updater plugin reads the rest.
#[derive(Deserialize)]
struct UpdaterConfig {
    #[serde(default)]
    pubkey: String,
}

/// The directory is signed with the update key, which tauri.conf.json holds as the public key file in base64.
/// An empty key means this build trusts no directory yet.
pub fn signing_key(
    plugins: &HashMap<String, serde_json::Value>,
) -> Result<Option<PublicKey>, ConfigError> {
    let Some(updater) = plugins.get("updater") else {
        return Ok(None);
    };
    let config: UpdaterConfig =
        serde_json::from_value(updater.clone()).map_err(|_| ConfigError::Key)?;
    decode_signing_key(&config.pubkey)
}

pub fn decode_signing_key(pubkey: &str) -> Result<Option<PublicKey>, ConfigError> {
    let pubkey = pubkey.trim();
    if pubkey.is_empty() {
        return Ok(None);
    }
    let text = STANDARD
        .decode(pubkey)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(ConfigError::Key)?;
    PublicKey::decode(&text)
        .map(Some)
        .map_err(|_| ConfigError::Key)
}

/// The launcher's own folders, plus the user's local app data folder, where the game keeps its profile.
#[derive(Debug, Clone)]
pub struct AppDirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub log: PathBuf,
    pub local_data: PathBuf,
}

impl AppDirs {
    pub fn resolve<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<Self> {
        use tauri::Manager;
        let path = app.path();
        Ok(Self {
            config: path.app_config_dir()?,
            data: path.app_data_dir()?,
            cache: path.app_cache_dir()?.join("cache"),
            log: path.app_log_dir()?,
            local_data: path.local_data_dir()?,
        })
    }

    pub fn create(&self) -> io::Result<()> {
        for dir in [&self.config, &self.data, &self.cache, &self.log] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// Writes through a sibling temp file and a rename, so a crash leaves the old file or the new one, never a torn one.
/// Each write gets a temp name of its own, so two writers of one file never trip over each other.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("the path has no parent folder"))?;
    fs::create_dir_all(parent)?;
    let mut prefix = file_name(path)?.to_os_string();
    prefix.push(".");
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .make_in(parent, |temp| {
            OpenOptions::new().write(true).create_new(true).open(temp)
        })?;
    temp.write_all(bytes)?;
    // The handle is closed before the rename, so a reader or a second writer never finds the target held open.
    let temp = temp.into_temp_path();
    fs::rename(&temp, path)
}

fn file_name(path: &Path) -> io::Result<&std::ffi::OsStr> {
    path.file_name()
        .ok_or_else(|| io::Error::other("the path has no file name"))
}

/// How a JSON file of the launcher's own was read.
pub enum StoreFile<T> {
    Missing,
    Read(T),
    /// The file was renamed to `<name>.bad` because this launcher cannot use it; the text says why and is fit for a player.
    SetAside(String),
}

/// Only the field every launcher-owned file carries, read before the full shape.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaOnly {
    schema_version: u32,
}

/// The schema version a JSON document announces, read before its full shape so another version gets a plain answer.
pub fn schema_version_of(bytes: &[u8]) -> Option<u32> {
    serde_json::from_slice::<SchemaOnly>(bytes)
        .ok()
        .map(|head| head.schema_version)
}

/// A damaged file, or one of another schema version, is set aside rather than stopping the launcher; nothing is deleted.
pub fn read_store<T: DeserializeOwned>(
    path: &Path,
    schema_version: u32,
) -> io::Result<StoreFile<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StoreFile::Missing),
        Err(error) => return Err(error),
    };
    let problem = match serde_json::from_slice::<SchemaOnly>(&bytes) {
        Ok(head) if head.schema_version != schema_version => format!(
            "it has schema version {}, this launcher knows {schema_version}",
            head.schema_version
        ),
        Ok(_) => match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => return Ok(StoreFile::Read(value)),
            Err(error) => error.to_string(),
        },
        Err(error) => error.to_string(),
    };
    let name = file_name(path)?.to_string_lossy().into_owned();
    let mut set_aside = file_name(path)?.to_os_string();
    set_aside.push(SET_ASIDE_SUFFIX);
    fs::rename(path, path.with_file_name(&set_aside))?;
    Ok(StoreFile::SetAside(format!(
        "{name} could not be read ({problem}), so it was renamed to {name}{SET_ASIDE_SUFFIX} and the launcher starts over without it."
    )))
}

/// The base folder when the player has not chosen one: the product's name at the root of the drive holding the game.
pub fn default_base_folder(game: &Path, product: &str) -> Option<PathBuf> {
    let Some(Component::Prefix(prefix)) = game.components().next() else {
        return None;
    };
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push("\\");
    root.push(product);
    Some(root)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Milliseconds since the Unix epoch, the unit every `expiresAt` from a backend uses.
pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    schema_version: u32,
    pub base_folder: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("settings.json: {0}")]
    Io(#[from] io::Error),
    #[error("settings.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the base folder must be an absolute path")]
    NotAbsolute,
    #[error("the base folder path points at a file")]
    NotAFolder,
}

pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    set_aside: Option<String>,
}

impl SettingsStore {
    pub fn load(config_dir: &Path) -> Result<Self, SettingsError> {
        let path = config_dir.join(SETTINGS_FILE);
        let (settings, set_aside) = match read_store(&path, SETTINGS_SCHEMA_VERSION)? {
            StoreFile::Read(settings) => (settings, None),
            StoreFile::Missing => (Settings::default(), None),
            StoreFile::SetAside(reason) => (Settings::default(), Some(reason)),
        };
        Ok(Self {
            path,
            settings,
            set_aside,
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Why the file on disk was set aside at load, given out once so the UI shows it once.
    pub fn take_set_aside(&mut self) -> Option<String> {
        self.set_aside.take()
    }

    /// An empty value clears the choice, so the default folder on the game's drive is used again.
    pub fn set_base_folder(&mut self, folder: &str) -> Result<(), SettingsError> {
        let folder = folder.trim();
        let path = Path::new(folder);
        if folder.is_empty() {
            self.settings.base_folder = None;
        } else if !path.is_absolute() {
            return Err(SettingsError::NotAbsolute);
        } else if path.is_file() {
            return Err(SettingsError::NotAFolder);
        } else {
            self.settings.base_folder = Some(folder.to_string());
        }
        write_atomic(&self.path, &serde_json::to_vec_pretty(&self.settings)?)?;
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            base_folder: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_config_is_https_only() {
        let config = LauncherConfig::bundled().unwrap();
        assert_eq!(config.directory_url.scheme(), "https");
        assert!(config.directory_url.path().ends_with("/directory.json"));
        assert_eq!(config.website.scheme(), "https");
        assert_eq!(config.rules_url.scheme(), "https");
    }

    #[test]
    fn an_http_directory_is_refused() {
        let text = r#"{"directoryUrl":"http://d.test/directory.json","website":"https://w.test/","rulesUrl":"https://w.test/rules"}"#;
        assert!(matches!(
            LauncherConfig::parse(text),
            Err(ConfigError::NotHttps("directoryUrl"))
        ));
    }

    #[test]
    fn an_unknown_key_is_refused_and_the_comment_is_not() {
        let with_comment = r#"{"$comment":"x","directoryUrl":"https://d.test/directory.json","website":"https://w.test/","rulesUrl":"https://w.test/rules"}"#;
        assert!(LauncherConfig::parse(with_comment).is_ok());
        let extra = r#"{"directoryUrl":"https://d.test/directory.json","website":"https://w.test/","rulesUrl":"https://w.test/rules","telemetry":true}"#;
        assert!(matches!(
            LauncherConfig::parse(extra),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn signing_key_reads_the_updater_entry() {
        let mut plugins = HashMap::new();
        assert!(signing_key(&plugins).unwrap().is_none());
        plugins.insert("updater".to_string(), serde_json::json!({ "pubkey": "" }));
        assert!(signing_key(&plugins).unwrap().is_none());
        plugins.insert(
            "updater".to_string(),
            serde_json::json!({ "pubkey": "bm90IGEga2V5" }),
        );
        assert!(matches!(signing_key(&plugins), Err(ConfigError::Key)));
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let encoded = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
        plugins.insert(
            "updater".to_string(),
            serde_json::json!({ "pubkey": encoded, "endpoints": [] }),
        );
        assert!(signing_key(&plugins).unwrap().is_some());
    }

    #[test]
    fn settings_default_then_persist_the_base_folder() {
        let root = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::load(root.path()).unwrap();
        assert_eq!(store.settings().base_folder, None);
        assert_eq!(store.take_set_aside(), None);
        assert!(matches!(
            store.set_base_folder("games"),
            Err(SettingsError::NotAbsolute)
        ));
        let file = root.path().join("a-file");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            store.set_base_folder(&file.to_string_lossy()),
            Err(SettingsError::NotAFolder)
        ));
        let folder = root.path().join("games");
        store.set_base_folder(&folder.to_string_lossy()).unwrap();
        let reloaded = SettingsStore::load(root.path()).unwrap();
        assert_eq!(
            reloaded.settings().base_folder.as_deref(),
            Some(folder.to_string_lossy().as_ref())
        );
        store.set_base_folder("  ").unwrap();
        assert_eq!(store.settings().base_folder, None);
        assert_eq!(
            SettingsStore::load(root.path())
                .unwrap()
                .settings()
                .base_folder,
            None
        );
    }

    #[test]
    fn a_foreign_settings_schema_is_set_aside_and_reported_once() {
        let root = tempfile::tempdir().unwrap();
        let newer = br#"{"schemaVersion":2,"baseFolder":null}"#;
        fs::write(root.path().join(SETTINGS_FILE), newer).unwrap();
        let mut store = SettingsStore::load(root.path()).unwrap();
        assert_eq!(store.settings(), &Settings::default());
        let reason = store.take_set_aside().unwrap();
        assert!(reason.contains("schema version 2"), "{reason}");
        assert!(reason.contains("settings.json.bad"), "{reason}");
        assert_eq!(store.take_set_aside(), None);
        assert!(!root.path().join(SETTINGS_FILE).exists());
        assert_eq!(
            fs::read(root.path().join("settings.json.bad")).unwrap(),
            newer
        );
    }

    #[test]
    fn a_damaged_settings_file_is_set_aside() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(SETTINGS_FILE), b"{\"schemaVersion\":1,").unwrap();
        let mut store = SettingsStore::load(root.path()).unwrap();
        assert!(store.take_set_aside().is_some());
        assert!(root.path().join("settings.json.bad").exists());
        let extra = br#"{"schemaVersion":1,"baseFolder":null,"theme":"dark"}"#;
        fs::write(root.path().join(SETTINGS_FILE), extra).unwrap();
        let mut store = SettingsStore::load(root.path()).unwrap();
        assert!(store.take_set_aside().unwrap().contains("theme"));
        assert_eq!(
            fs::read(root.path().join("settings.json.bad")).unwrap(),
            extra
        );
    }

    #[test]
    fn the_default_base_folder_sits_on_the_drive_of_the_game() {
        let game =
            Path::new("C:\\Program Files (x86)\\Steam\\steamapps\\common\\Skyrim Special Edition");
        assert_eq!(
            default_base_folder(game, "SUG Launcher"),
            Some(PathBuf::from("C:\\SUG Launcher"))
        );
        assert_eq!(
            default_base_folder(Path::new("D:\\SteamLibrary\\steamapps\\common\\x"), "Fork"),
            Some(PathBuf::from("D:\\Fork"))
        );
        assert_eq!(
            default_base_folder(Path::new("relative\\path"), "Fork"),
            None
        );
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nested").join("file.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let left: Vec<_> = fs::read_dir(root.path().join("nested"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["file.json"]);
    }

    #[test]
    fn two_writers_of_one_file_never_trip_over_each_other() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.json");
        let path = &path;
        std::thread::scope(|scope| {
            for text in [b"one".as_slice(), b"two".as_slice()] {
                scope.spawn(move || {
                    for _ in 0..200 {
                        write_atomic(path, text).unwrap();
                    }
                });
            }
        });
        assert!(matches!(
            fs::read(path).unwrap().as_slice(),
            b"one" | b"two"
        ));
    }
}

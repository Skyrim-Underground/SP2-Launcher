// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-server game folder: Steam files linked in, manifest files downloaded into place, and the two records of what was placed and hashed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};
use windows::core::HSTRING;

use crate::config::{StoreFile, read_store, write_atomic};
use crate::download::{
    Cancel, DownloadError, Downloader, PART_SUFFIX, Request, remove_if_present, sha256_of_file,
};
use crate::game::{self, RuntimeError};
use crate::manifest::{Manifest, PathError, Source, relative_path};

pub const SERVERS_DIR: &str = "servers";
pub const INSTALLED_FILE: &str = "installed.json";
pub const HASHES_FILE: &str = "hashes.json";
pub const INSTALLED_SCHEMA_VERSION: u32 = 1;
pub const HASHES_SCHEMA_VERSION: u32 = 1;

/// Where one server's files live: its game folder under the base folder and its hash cache under the app cache.
#[derive(Debug, Clone)]
pub struct Folders {
    pub game: PathBuf,
    pub cache: PathBuf,
}

impl Folders {
    pub fn new(base: &Path, cache_root: &Path, id: &str) -> Self {
        Self {
            game: base.join(SERVERS_DIR).join(id),
            cache: cache_root.join(id),
        }
    }
}

/// What the launcher placed in the game folder itself; runtime links are not recorded, the Steam copy defines them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installed {
    schema_version: u32,
    pub manifest_version: String,
    pub files: BTreeMap<String, InstalledFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledFile {
    pub sha256: String,
    pub size: u64,
}

impl Default for Installed {
    fn default() -> Self {
        Self {
            schema_version: INSTALLED_SCHEMA_VERSION,
            manifest_version: String::new(),
            files: BTreeMap::new(),
        }
    }
}

/// A file's hash is trusted while its size and modification time are the ones it was hashed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HashCache {
    schema_version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CacheEntry {
    size: u64,
    mtime_ns: u64,
    sha256: String,
}

impl Default for HashCache {
    fn default() -> Self {
        Self {
            schema_version: HASHES_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "phase",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Phase {
    Planning,
    Removing {
        done: usize,
        total: usize,
    },
    Linking {
        done: usize,
        total: usize,
    },
    Downloading {
        file: String,
        done: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
    },
    Finishing,
}

pub type ProgressHandle = Arc<Mutex<Phase>>;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the install was cancelled")]
    Cancelled,
    #[error("not enough free space: {needed} bytes are needed and {free} are free")]
    NoSpace { needed: u64, free: u64 },
    #[error("{path}: {source}")]
    Download { path: String, source: DownloadError },
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("the path {path:?} is not allowed: {reason}")]
    Path { path: String, reason: PathError },
    #[error("{path:?} comes from an archive, which this launcher cannot install")]
    Archive { path: String },
    #[error("the install record could not be written: {0}")]
    Record(#[from] serde_json::Error),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

fn io_at(path: &Path, source: io::Error) -> InstallError {
    InstallError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    }
}

fn checked(path: &str) -> Result<PathBuf, InstallError> {
    relative_path(path).map_err(|reason| InstallError::Path {
        path: path.to_string(),
        reason,
    })
}

/// One of the launcher's own records, or a fresh one when there is none; a file set aside is reported through `notices`.
fn read_record<T: Default + DeserializeOwned>(
    path: &Path,
    schema_version: u32,
    notices: &mut Vec<String>,
) -> Result<T, InstallError> {
    match read_store::<T>(path, schema_version).map_err(|error| io_at(path, error))? {
        StoreFile::Read(record) => Ok(record),
        StoreFile::Missing => Ok(T::default()),
        StoreFile::SetAside(reason) => {
            notices.push(reason);
            Ok(T::default())
        }
    }
}

/// A file of the Steam copy that the game folder gets as a link or a copy.
#[derive(Debug, Clone)]
pub struct RuntimeFile {
    pub relative: PathBuf,
    pub size: u64,
    /// Already in the game folder with the same size when the plan was made.
    pub present: bool,
}

/// What an install will do, decided from the manifest, the records and the files on disk.
pub struct Plan {
    pub download: Vec<usize>,
    pub keep: Vec<usize>,
    pub remove: Vec<String>,
    pub runtime: Vec<RuntimeFile>,
    pub copy_runtime: bool,
    pub bytes: u64,
    installed: Installed,
    cache: HashCache,
    notices: Vec<String>,
}

impl Plan {
    pub fn installed_version(&self) -> Option<&str> {
        (!self.installed.manifest_version.is_empty()).then_some(&self.installed.manifest_version)
    }

    /// Bytes the install must have room for: the downloads, plus the runtime files when they are copied.
    pub fn needed_bytes(&self) -> u64 {
        let runtime = if self.copy_runtime {
            self.runtime
                .iter()
                .filter(|file| !file.present)
                .map(|file| file.size)
                .sum()
        } else {
            0
        };
        self.bytes + runtime
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub downloaded: usize,
    pub kept: usize,
    pub removed: usize,
    pub linked: usize,
    pub copied: usize,
    pub notices: Vec<String>,
}

pub struct Installer {
    steam: PathBuf,
    folders: Folders,
    cancel: Cancel,
    progress: ProgressHandle,
}

impl Installer {
    pub fn new(steam: PathBuf, folders: Folders, cancel: Cancel, progress: ProgressHandle) -> Self {
        Self {
            steam,
            folders,
            cancel,
            progress,
        }
    }

    fn set_phase(&self, phase: Phase) {
        *self
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = phase;
    }

    fn check_cancel(&self) -> Result<(), InstallError> {
        if self.cancel.is_cancelled() {
            Err(InstallError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn installed_path(&self) -> PathBuf {
        self.folders.game.join(INSTALLED_FILE)
    }

    fn hashes_path(&self) -> PathBuf {
        self.folders.cache.join(HASHES_FILE)
    }

    /// Decides every file's fate without writing anything; files on disk are hashed through the cache, or in full when `verify` is set.
    pub fn plan(&self, manifest: &Manifest, verify: bool) -> Result<Plan, InstallError> {
        self.set_phase(Phase::Planning);
        let mut notices = Vec::new();
        let installed: Installed = read_record(
            &self.installed_path(),
            INSTALLED_SCHEMA_VERSION,
            &mut notices,
        )?;
        let mut cache = if verify {
            HashCache::default()
        } else {
            read_record(&self.hashes_path(), HASHES_SCHEMA_VERSION, &mut notices)?
        };
        let mut download = Vec::new();
        let mut keep = Vec::new();
        let mut bytes = 0;
        for (index, file) in manifest.files.iter().enumerate() {
            if let Source::Archive { .. } = file.source {
                return Err(InstallError::Archive {
                    path: file.to.clone(),
                });
            }
            self.check_cancel()?;
            let dest = self.folders.game.join(checked(&file.to)?);
            let matches = match fs::metadata(&dest) {
                Ok(meta) if meta.is_file() && meta.len() == file.size => {
                    cache
                        .hash_of(&file.to, &dest, &meta)
                        .map_err(|error| io_at(&dest, error))?
                        == file.sha256
                }
                _ => false,
            };
            if matches {
                keep.push(index);
            } else {
                download.push(index);
                bytes += file.size;
            }
        }
        let named = manifest.named_paths();
        let remove = installed
            .files
            .keys()
            .filter(|path| !named.contains(&path.to_lowercase()))
            .cloned()
            .collect();
        let mut runtime = Vec::new();
        walk(&self.steam, Path::new(""), &mut runtime)
            .map_err(|error| io_at(&self.steam, error))?;
        runtime.retain(|file| !named.contains(&lower_key(&file.relative)));
        for file in &mut runtime {
            file.present = same_size_file(&self.folders.game.join(&file.relative), file.size);
        }
        let copy_runtime = volume_of(&self.folders.game) != volume_of(&self.steam);
        Ok(Plan {
            download,
            keep,
            remove,
            runtime,
            copy_runtime,
            bytes,
            installed,
            cache,
            notices,
        })
    }

    /// The server's hashed runtime files as they are in the game folder, checked through the hash cache before a Play.
    /// A cache file that had to be set aside is reported through `notices`, whichever way the check ends.
    pub fn check_runtime(
        &self,
        manifest: &Manifest,
        notices: &mut Vec<String>,
    ) -> Result<(), InstallError> {
        let mut cache: HashCache =
            read_record(&self.hashes_path(), HASHES_SCHEMA_VERSION, notices)?;
        let before = cache.clone();
        let outcome = game::check_runtime_files(
            &self.folders.game,
            &manifest.runtime.files,
            &mut |key, path, meta| cache.hash_of(key, path, meta),
        );
        if cache != before {
            cache.save(&self.hashes_path())?;
        }
        outcome.map_err(InstallError::Runtime)
    }

    /// Carries the plan out; the records are written however it ends, so they describe the folder as it is.
    pub async fn run(
        &self,
        manifest: &Manifest,
        mut plan: Plan,
        downloader: &Downloader,
    ) -> Result<Report, InstallError> {
        let mut report = Report {
            kept: plan.keep.len(),
            notices: std::mem::take(&mut plan.notices),
            ..Report::default()
        };
        let outcome = self
            .execute(manifest, &mut plan, downloader, &mut report)
            .await;
        if outcome.is_ok() {
            plan.installed.manifest_version = manifest.version.clone();
        }
        plan.installed.save(&self.installed_path())?;
        plan.cache.save(&self.hashes_path())?;
        outcome.map(|()| report)
    }

    async fn execute(
        &self,
        manifest: &Manifest,
        plan: &mut Plan,
        downloader: &Downloader,
        report: &mut Report,
    ) -> Result<(), InstallError> {
        // The game folder is the first thing an install creates; the free-space check needs it to exist.
        fs::create_dir_all(&self.folders.game).map_err(|error| io_at(&self.folders.game, error))?;
        let needed = plan.needed_bytes();
        let free =
            free_space(&self.folders.game).map_err(|error| io_at(&self.folders.game, error))?;
        if needed > free {
            return Err(InstallError::NoSpace { needed, free });
        }
        self.remove_dropped(plan, report)?;
        self.place_runtime(plan, report)?;
        self.download_files(manifest, plan, downloader, report)
            .await
    }

    /// Only files the record says the launcher placed go; anything the player added stays.
    fn remove_dropped(&self, plan: &mut Plan, report: &mut Report) -> Result<(), InstallError> {
        let total = plan.remove.len();
        for (done, path) in plan.remove.iter().enumerate() {
            self.set_phase(Phase::Removing { done, total });
            self.check_cancel()?;
            let dest = self.folders.game.join(checked(path)?);
            remove_if_present(&dest).map_err(|error| io_at(&dest, error))?;
            plan.installed.files.remove(path);
            plan.cache.entries.remove(path);
            report.removed += 1;
        }
        Ok(())
    }

    /// A link costs no space and never changes the Steam copy, since the launcher only creates or deletes links.
    fn place_runtime(&self, plan: &Plan, report: &mut Report) -> Result<(), InstallError> {
        let total = plan.runtime.len();
        for (done, file) in plan.runtime.iter().enumerate() {
            self.set_phase(Phase::Linking { done, total });
            self.check_cancel()?;
            let dest = self.folders.game.join(&file.relative);
            if same_size_file(&dest, file.size) {
                continue;
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|error| io_at(parent, error))?;
            }
            remove_if_present(&dest).map_err(|error| io_at(&dest, error))?;
            let source = self.steam.join(&file.relative);
            if plan.copy_runtime {
                fs::copy(&source, &dest).map_err(|error| io_at(&dest, error))?;
                report.copied += 1;
            } else {
                fs::hard_link(&source, &dest).map_err(|error| io_at(&dest, error))?;
                report.linked += 1;
            }
        }
        Ok(())
    }

    /// Each file lands as a .part next to its place and is renamed over whatever was there, a vanilla link included.
    async fn download_files(
        &self,
        manifest: &Manifest,
        plan: &mut Plan,
        downloader: &Downloader,
        report: &mut Report,
    ) -> Result<(), InstallError> {
        let total = plan.download.len();
        let bytes_total = plan.bytes;
        let mut bytes_before = 0;
        for (done, index) in plan.download.iter().enumerate() {
            let file = &manifest.files[*index];
            let Source::Url { url } = &file.source else {
                return Err(InstallError::Archive {
                    path: file.to.clone(),
                });
            };
            self.check_cancel()?;
            let dest = self.folders.game.join(checked(&file.to)?);
            let part = part_path(&dest);
            let phase = |bytes_done| Phase::Downloading {
                file: file.to.clone(),
                done,
                total,
                bytes_done,
                bytes_total,
            };
            self.set_phase(phase(bytes_before));
            let request = Request {
                url,
                origins: &manifest.origins,
                part: &part,
                size: file.size,
                sha256: &file.sha256,
            };
            let progress = self.progress.clone();
            let mut on_progress = move |file_bytes: u64| {
                *progress
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    phase(bytes_before + file_bytes);
            };
            downloader
                .fetch(&request, &self.cancel, &mut on_progress)
                .await
                .map_err(|source| match source {
                    DownloadError::Cancelled => InstallError::Cancelled,
                    source => InstallError::Download {
                        path: file.to.clone(),
                        source,
                    },
                })?;
            remove_if_present(&dest).map_err(|error| io_at(&dest, error))?;
            fs::rename(&part, &dest).map_err(|error| io_at(&dest, error))?;
            let meta = fs::metadata(&dest).map_err(|error| io_at(&dest, error))?;
            plan.cache.remember(&file.to, &meta, &file.sha256);
            plan.installed.files.insert(
                file.to.clone(),
                InstalledFile {
                    sha256: file.sha256.clone(),
                    size: file.size,
                },
            );
            report.downloaded += 1;
            bytes_before += file.size;
        }
        self.set_phase(Phase::Finishing);
        Ok(())
    }
}

impl Installed {
    fn save(&self, path: &Path) -> Result<(), InstallError> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?).map_err(|error| io_at(path, error))
    }
}

impl HashCache {
    /// The cached hash when size and mtime still match, otherwise a fresh full hash that replaces the entry.
    fn hash_of(&mut self, key: &str, path: &Path, meta: &fs::Metadata) -> io::Result<String> {
        let mtime_ns = mtime_ns(meta);
        if let Some(entry) = self.entries.get(key)
            && entry.size == meta.len()
            && entry.mtime_ns == mtime_ns
        {
            return Ok(entry.sha256.clone());
        }
        let sha256 = sha256_of_file(path)?;
        self.remember(key, meta, &sha256);
        Ok(sha256)
    }

    fn remember(&mut self, key: &str, meta: &fs::Metadata, sha256: &str) {
        self.entries.insert(
            key.to_string(),
            CacheEntry {
                size: meta.len(),
                mtime_ns: mtime_ns(meta),
                sha256: sha256.to_string(),
            },
        );
    }

    fn save(&self, path: &Path) -> Result<(), InstallError> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?).map_err(|error| io_at(path, error))
    }
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_nanos() as u64)
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    dest.with_file_name(name)
}

fn same_size_file(path: &Path, size: u64) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == size)
}

/// The relative path as the manifest would spell it, lower-cased for comparison.
fn lower_key(relative: &Path) -> String {
    lower_components(relative).join("/")
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// True when the two folders are one or one lies inside the other; Windows paths compare without case.
pub fn overlapping(a: &Path, b: &Path) -> bool {
    let a = lower_components(a);
    let b = lower_components(b);
    a.starts_with(&b) || b.starts_with(&a)
}

/// Every regular file below the root, in name order; links and junctions are left out.
fn walk(root: &Path, relative: &Path, out: &mut Vec<RuntimeFile>) -> io::Result<()> {
    let mut entries: Vec<fs::DirEntry> =
        fs::read_dir(root.join(relative))?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = relative.join(entry.file_name());
        if file_type.is_dir() {
            walk(root, &path, out)?;
        } else if file_type.is_file() {
            out.push(RuntimeFile {
                relative: path,
                size: entry.metadata()?.len(),
                present: false,
            });
        }
    }
    Ok(())
}

/// The mount point a path sits on, so two folders on one volume can share files by hard link.
fn volume_of(path: &Path) -> Option<String> {
    let name = HSTRING::from(path.as_os_str());
    let mut buffer = vec![0u16; 1024];
    // SAFETY: the name outlives the call and the buffer length is passed with it.
    unsafe { GetVolumePathNameW(&name, &mut buffer) }.ok()?;
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]).to_lowercase())
}

fn free_space(path: &Path) -> io::Result<u64> {
    let name = HSTRING::from(path.as_os_str());
    let mut free: u64 = 0;
    // SAFETY: the name outlives the call and the out pointer is a live local.
    unsafe { GetDiskFreeSpaceExW(&name, Some(&mut free), None, None) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_cache_hits_on_size_and_mtime_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.esp");
        fs::write(&file, b"abc").unwrap();
        let mut cache = HashCache::default();
        let meta = fs::metadata(&file).unwrap();
        let first = cache.hash_of("Data/a.esp", &file, &meta).unwrap();
        assert_eq!(
            first,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Same size and mtime, so the stale cached value is what comes back without a read.
        cache.entries.get_mut("Data/a.esp").unwrap().sha256 = "cached".into();
        assert_eq!(cache.hash_of("Data/a.esp", &file, &meta).unwrap(), "cached");
        let later = UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let meta = fs::metadata(&file).unwrap();
        assert_eq!(cache.hash_of("Data/a.esp", &file, &meta).unwrap(), first);
        let path = dir.path().join("hashes.json");
        cache.save(&path).unwrap();
        let reloaded = match read_store::<HashCache>(&path, HASHES_SCHEMA_VERSION).unwrap() {
            StoreFile::Read(cache) => cache,
            _ => panic!("the cache must read back"),
        };
        assert_eq!(reloaded, cache);
    }

    #[test]
    fn part_paths_and_keys_follow_the_destination() {
        assert_eq!(
            part_path(Path::new("C:\\base\\servers\\x\\Data\\Skyrim.esm")),
            PathBuf::from("C:\\base\\servers\\x\\Data\\Skyrim.esm.part")
        );
        assert_eq!(
            lower_key(Path::new("Data").join("SKSE").join("X.DLL").as_path()),
            "data/skse/x.dll"
        );
        assert_eq!(
            Folders::new(
                Path::new("C:\\base"),
                Path::new("C:\\cache"),
                "api.example.org"
            )
            .game,
            PathBuf::from("C:\\base\\servers\\api.example.org")
        );
    }

    #[test]
    fn folders_overlap_when_one_holds_the_other() {
        let steam = Path::new(r"C:\Steam\steamapps\common\Skyrim Special Edition");
        assert!(overlapping(steam, steam));
        assert!(overlapping(
            steam,
            Path::new(r"c:\steam\steamapps\common\skyrim special edition\servers\x")
        ));
        assert!(overlapping(Path::new(r"C:\Steam\steamapps"), steam));
        assert!(!overlapping(
            steam,
            Path::new(r"C:\Steam\steamapps\common\Skyrim Special Edition 2\servers\x")
        ));
        assert!(!overlapping(steam, Path::new(r"C:\SUG Launcher\servers\x")));
        assert!(!overlapping(
            steam,
            Path::new(r"D:\Steam\steamapps\common\Skyrim Special Edition")
        ));
    }

    #[test]
    fn the_walk_lists_regular_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Data").join("Scripts")).unwrap();
        fs::write(dir.path().join("b.exe"), b"bb").unwrap();
        fs::write(dir.path().join("a.dll"), b"a").unwrap();
        fs::write(dir.path().join("Data").join("Skyrim.esm"), b"esm").unwrap();
        let mut files = Vec::new();
        walk(dir.path(), Path::new(""), &mut files).unwrap();
        let listed: Vec<(String, u64)> = files
            .iter()
            .map(|file| (lower_key(&file.relative), file.size))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("data/skyrim.esm".to_string(), 3),
                ("a.dll".to_string(), 1),
                ("b.exe".to_string(), 2)
            ]
        );
    }

    #[test]
    fn volumes_and_free_space_are_read_for_a_real_folder() {
        let dir = tempfile::tempdir().unwrap();
        let volume = volume_of(dir.path()).unwrap();
        assert!(volume.ends_with('\\'), "{volume}");
        assert_eq!(volume_of(&dir.path().join("deeper")), Some(volume));
        assert!(free_space(dir.path()).unwrap() > 0);
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads and the per-server folder against the Node static file server in test/, and against the Steam copy when asked.

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::NodeServer;
use sug_launcher_lib::deploy::{Folders, InstallError, Installed, Installer, Phase, Report};
use sug_launcher_lib::download::{
    Cancel, DownloadError, Downloader, Received, Request, sha256_of_file,
};
use sug_launcher_lib::game::{self, RuntimeError};
use sug_launcher_lib::manifest::{Manifest, RuntimeFile, is_native_name};
use tempfile::TempDir;
use url::Url;

const BIG: u64 = 64 * 1024 * 3 + 7;
const SERVER_ID: &str = "fake.example.org";

/// A folder of files served by the static server.
struct Served {
    dir: TempDir,
    server: NodeServer,
}

impl Served {
    fn new(files: &[(&str, &[u8])], extra_args: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        let root = dir.path().to_string_lossy().into_owned();
        let mut args = vec!["--root", root.as_str()];
        args.extend_from_slice(extra_args);
        let server = NodeServer::start("static-files.mjs", &args);
        Self { dir, server }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.dir.path().join(name), bytes).unwrap();
    }

    fn hash(&self, name: &str) -> String {
        sha256_of_file(&self.dir.path().join(name)).unwrap()
    }

    fn origin(&self) -> Url {
        self.server.origin.clone()
    }

    fn url(&self, route: &str, name: &str) -> Url {
        self.server
            .origin
            .join(&format!("/{route}/{name}"))
            .unwrap()
    }
}

fn big_bytes() -> Vec<u8> {
    (0..BIG).map(|i| (i * 7 % 251) as u8).collect()
}

fn downloader() -> Downloader {
    Downloader::new("sug-launcher-tests/0").unwrap()
}

/// One fetch with the test client; the last progress value is returned with the outcome.
async fn fetch(
    url: &Url,
    origins: &[Url],
    part: &Path,
    size: u64,
    sha256: &str,
) -> (Result<Received, DownloadError>, u64) {
    let mut last = 0;
    let result = downloader()
        .fetch(
            &Request {
                url,
                origins,
                part,
                size,
                sha256,
            },
            &Cancel::default(),
            &mut |done| last = done,
        )
        .await;
    (result, last)
}

/// A stand-in for the Steam copy: an exe, two masters and a script.
fn fake_steam() -> TempDir {
    let steam = tempfile::tempdir().unwrap();
    fs::create_dir_all(steam.path().join("Data").join("Scripts")).unwrap();
    fs::write(steam.path().join("SkyrimSE.exe"), b"exe bytes").unwrap();
    fs::write(steam.path().join("Data").join("Skyrim.esm"), b"skyrim").unwrap();
    fs::write(steam.path().join("Data").join("Update.esm"), b"update").unwrap();
    fs::write(
        steam.path().join("Data").join("Scripts").join("a.pex"),
        b"pex",
    )
    .unwrap();
    steam
}

/// A manifest read as a shape without the rules, because the test server speaks plain http.
fn manifest_for(served: &Served, version: &str, files: &[(&str, &str)]) -> Manifest {
    let entries: Vec<String> = files
        .iter()
        .map(|(to, name)| {
            let size = fs::metadata(served.dir.path().join(name)).unwrap().len();
            format!(
                r#"{{"to":"{to}","size":{size},"sha256":"{}","native":{},"source":{{"kind":"url","url":"{}"}}}}"#,
                served.hash(name),
                is_native_name(to),
                served.url("files", name)
            )
        })
        .collect();
    let text = format!(
        r#"{{"schemaVersion":1,"version":"{version}","gameRuntime":"skyrimse-1.7.104","protocolId":"sug1_","runtime":{{"exe":"SkyrimSE.exe","files":[]}},"launch":{{"loader":"skse64_loader.exe"}},"origins":["{}"],"files":[{}],"plugins":[]}}"#,
        served.origin().as_str().trim_end_matches('/'),
        entries.join(",")
    );
    serde_json::from_str(&text).unwrap()
}

fn installer(steam: &Path, base: &Path, cache: &Path) -> (Installer, Arc<Mutex<Phase>>, Cancel) {
    let progress = Arc::new(Mutex::new(Phase::Planning));
    let cancel = Cancel::default();
    let installer = Installer::new(
        steam.to_path_buf(),
        Folders::new(base, cache, SERVER_ID),
        cancel.clone(),
        progress.clone(),
    );
    (installer, progress, cancel)
}

async fn install(
    installer: &Installer,
    manifest: &Manifest,
    verify: bool,
) -> Result<Report, InstallError> {
    let plan = installer.plan(manifest, verify)?;
    installer.run(manifest, plan, &downloader()).await
}

fn game_folder(base: &Path) -> PathBuf {
    base.join("servers").join(SERVER_ID)
}

fn record(base: &Path) -> Installed {
    serde_json::from_slice(&fs::read(game_folder(base).join("installed.json")).unwrap()).unwrap()
}

fn set_modified(path: &Path, time: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[tokio::test]
async fn a_size_overrun_aborts_mid_stream() {
    let big = big_bytes();
    let served = Served::new(&[("big.bin", &big)], &[]);
    let hash = served.hash("big.bin");
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("big.bin.part");
    let origins = [served.origin()];
    let (announced, _) = fetch(
        &served.url("files", "big.bin"),
        &origins,
        &part,
        1000,
        &hash,
    )
    .await;
    assert!(
        matches!(announced, Err(DownloadError::SizeExceeded { size: 1000 })),
        "{announced:?}"
    );
    assert!(!part.exists());
    // The overrun route never stops sending, so only a client that aborts at the stated size returns at all.
    let (endless, last) = tokio::time::timeout(
        Duration::from_secs(20),
        fetch(
            &served.url("overrun", "big.bin"),
            &origins,
            &part,
            BIG,
            &hash,
        ),
    )
    .await
    .expect("the download must stop the moment the stated size is exceeded");
    assert!(
        matches!(endless, Err(DownloadError::SizeExceeded { size }) if size == BIG),
        "{endless:?}"
    );
    assert!(
        last <= BIG,
        "{last} bytes were written past the stated size"
    );
    assert!(!part.exists());
}

#[tokio::test]
async fn a_redirect_off_the_listed_origins_is_refused_before_any_contact() {
    let listed = Served::new(&[("a.esp", b"aaa")], &[]);
    let listed_origin = listed.origin().as_str().trim_end_matches('/').to_string();
    let to_dead_port = Served::new(&[], &["--redirect-to", "http://127.0.0.1:1"]);
    let to_listed = Served::new(&[], &["--redirect-to", &listed_origin]);
    let hash = listed.hash("a.esp");
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("a.esp.part");
    // Nothing listens on port 1, so a connection there would fail; the list refuses the hop first.
    let (result, _) = fetch(
        &to_dead_port.url("elsewhere", "a.esp"),
        &[to_dead_port.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert!(
        matches!(result, Err(DownloadError::OffOrigin { ref host }) if host == "127.0.0.1"),
        "{result:?}"
    );
    assert!(!part.exists());
    let (result, _) = fetch(
        &to_listed.url("elsewhere", "a.esp"),
        &[to_listed.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert!(
        matches!(result, Err(DownloadError::OffOrigin { .. })),
        "{result:?}"
    );
    let (result, _) = fetch(
        &to_listed.url("elsewhere", "a.esp"),
        &[to_listed.origin(), listed.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert_eq!(
        result.unwrap(),
        Received {
            resumed_from: 0,
            bytes: 3
        }
    );
    fs::remove_file(&part).unwrap();
    let (result, _) = fetch(
        &listed.url("moved", "a.esp"),
        &[listed.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert_eq!(result.unwrap().bytes, 3);
    fs::remove_file(&part).unwrap();
    let (result, _) = fetch(
        &listed.url("loop", "a.esp"),
        &[listed.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert!(
        matches!(result, Err(DownloadError::TooManyRedirects)),
        "{result:?}"
    );
    let (result, _) = fetch(
        &listed.url("files", "a.esp"),
        &[to_dead_port.origin()],
        &part,
        3,
        &hash,
    )
    .await;
    assert!(
        matches!(result, Err(DownloadError::OffOrigin { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_killed_download_resumes_from_its_part() {
    let big = big_bytes();
    let served = Served::new(&[("big.bin", &big)], &[]);
    let hash = served.hash("big.bin");
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("big.bin.part");
    let origins = [served.origin()];
    let (cut, _) = fetch(
        &served.url("cut", "70000/big.bin"),
        &origins,
        &part,
        BIG,
        &hash,
    )
    .await;
    assert!(
        matches!(
            cut,
            Err(DownloadError::Network(_)) | Err(DownloadError::Incomplete { .. })
        ),
        "{cut:?}"
    );
    let kept = fs::metadata(&part).unwrap().len();
    assert!(kept > 0 && kept < BIG, "{kept} bytes kept");
    let (resumed, last) = fetch(&served.url("files", "big.bin"), &origins, &part, BIG, &hash).await;
    assert_eq!(
        resumed.unwrap(),
        Received {
            resumed_from: kept,
            bytes: BIG - kept
        }
    );
    assert_eq!(last, BIG);
    assert_eq!(sha256_of_file(&part).unwrap(), hash);
    let (again, _) = fetch(&served.url("files", "big.bin"), &origins, &part, BIG, &hash).await;
    assert_eq!(
        again.unwrap(),
        Received {
            resumed_from: BIG,
            bytes: 0
        }
    );
    // A part left by another version of the file fails its hash once and is then fetched whole.
    let stale: Vec<u8> = big[..5000].iter().map(|byte| byte ^ 0xff).collect();
    fs::write(&part, &stale).unwrap();
    let (replaced, _) = fetch(&served.url("files", "big.bin"), &origins, &part, BIG, &hash).await;
    assert_eq!(
        replaced.unwrap(),
        Received {
            resumed_from: 0,
            bytes: BIG
        }
    );
    assert_eq!(sha256_of_file(&part).unwrap(), hash);
}

#[tokio::test]
async fn a_changed_file_is_fetched_again_and_an_unchanged_one_kept() {
    let served = Served::new(&[("a.esp", b"first a"), ("b.esp", b"first b")], &[]);
    let steam = fake_steam();
    let base = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (installer, progress, _) = installer(steam.path(), base.path(), cache.path());
    let both = [("Data/a.esp", "a.esp"), ("Data/b.esp", "b.esp")];
    let v1 = manifest_for(&served, "1", &both);
    let report = install(&installer, &v1, false).await.unwrap();
    assert_eq!(
        report,
        Report {
            downloaded: 2,
            kept: 0,
            removed: 0,
            linked: 4,
            copied: 0,
            notices: Vec::new()
        }
    );
    assert!(matches!(*progress.lock().unwrap(), Phase::Finishing));
    let game = game_folder(base.path());
    let a = game.join("Data").join("a.esp");
    let b = game.join("Data").join("b.esp");
    assert_eq!(fs::read(&a).unwrap(), b"first a");
    assert!(game.join("Data").join("Scripts").join("a.pex").is_file());
    assert!(!game.join("Data").join("a.esp.part").exists());
    let installed = record(base.path());
    assert_eq!(installed.manifest_version, "1");
    assert_eq!(installed.files.len(), 2);
    assert_eq!(installed.files["Data/b.esp"].sha256, served.hash("b.esp"));
    let a_modified = fs::metadata(&a).unwrap().modified().unwrap();
    served.write("b.esp", b"second b");
    let v2 = manifest_for(&served, "2", &both);
    let report = install(&installer, &v2, false).await.unwrap();
    assert_eq!((report.downloaded, report.kept, report.linked), (1, 1, 0));
    assert_eq!(fs::read(&b).unwrap(), b"second b");
    assert_eq!(fs::metadata(&a).unwrap().modified().unwrap(), a_modified);
    assert_eq!(record(base.path()).manifest_version, "2");
    let report = install(&installer, &v2, true).await.unwrap();
    assert_eq!((report.downloaded, report.kept), (0, 2));
    // Same size and mtime pass the cache, so only Verify files sees the damage and repairs it.
    fs::write(&a, b"FIRST A").unwrap();
    set_modified(&a, a_modified);
    let report = install(&installer, &v2, false).await.unwrap();
    assert_eq!(report.kept, 2);
    let report = install(&installer, &v2, true).await.unwrap();
    assert_eq!((report.downloaded, report.kept), (1, 1));
    assert_eq!(fs::read(&a).unwrap(), b"first a");
}

#[tokio::test]
async fn a_dropped_entry_removes_only_what_the_launcher_placed() {
    let served = Served::new(&[("a.esp", b"first a"), ("b.esp", b"first b")], &[]);
    let steam = fake_steam();
    let base = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (installer, _, _) = installer(steam.path(), base.path(), cache.path());
    let v1 = manifest_for(
        &served,
        "1",
        &[("Data/a.esp", "a.esp"), ("Data/b.esp", "b.esp")],
    );
    install(&installer, &v1, false).await.unwrap();
    let game = game_folder(base.path());
    let a = game.join("Data").join("a.esp");
    let b = game.join("Data").join("b.esp");
    let player = game.join("Data").join("Player.esp");
    fs::write(&player, b"mine").unwrap();
    let v2 = manifest_for(&served, "2", &[("Data/a.esp", "a.esp")]);
    let report = install(&installer, &v2, false).await.unwrap();
    assert_eq!((report.removed, report.downloaded, report.kept), (1, 0, 1));
    assert!(!b.exists());
    assert_eq!(fs::read(&player).unwrap(), b"mine");
    assert_eq!(fs::read(&a).unwrap(), b"first a");
    let installed = record(base.path());
    assert_eq!(
        installed.files.keys().collect::<Vec<_>>(),
        vec!["Data/a.esp"]
    );
    fs::remove_file(&a).unwrap();
    let report = install(&installer, &v2, false).await.unwrap();
    assert_eq!((report.downloaded, report.removed), (1, 0));
    // A manifest may replace a vanilla file: the link goes, the download takes its place, the Steam copy stays.
    let update = game.join("Data").join("Update.esm");
    let v3 = manifest_for(&served, "3", &[("Data/Update.esm", "a.esp")]);
    let report = install(&installer, &v3, false).await.unwrap();
    assert_eq!(
        (report.downloaded, report.removed, report.linked),
        (1, 1, 0)
    );
    assert_eq!(fs::read(&update).unwrap(), b"first a");
    assert_eq!(
        fs::read(steam.path().join("Data").join("Update.esm")).unwrap(),
        b"update"
    );
    assert!(!a.exists());
    assert_eq!(fs::read(&player).unwrap(), b"mine");
    // Once the manifest stops naming it, the vanilla file is linked again.
    let v4 = manifest_for(&served, "4", &[]);
    let report = install(&installer, &v4, false).await.unwrap();
    assert_eq!(
        (report.removed, report.linked, report.downloaded),
        (1, 1, 0)
    );
    assert_eq!(fs::read(&update).unwrap(), b"update");
    assert!(record(base.path()).files.is_empty());
}

#[tokio::test]
async fn a_cancel_stops_before_the_next_file_and_the_records_stay_true() {
    let served = Served::new(&[("a.esp", b"first a")], &[]);
    let steam = fake_steam();
    let base = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (installer, _, cancel) = installer(steam.path(), base.path(), cache.path());
    let v1 = manifest_for(&served, "1", &[("Data/a.esp", "a.esp")]);
    let plan = installer.plan(&v1, false).unwrap();
    assert_eq!(
        (plan.download.len(), plan.keep.len(), plan.bytes),
        (1, 0, 7)
    );
    assert!(!game_folder(base.path()).exists());
    assert!(!cache.path().join(SERVER_ID).exists());
    cancel.cancel();
    let result = installer.run(&v1, plan, &downloader()).await;
    assert!(matches!(result, Err(InstallError::Cancelled)), "{result:?}");
    assert!(!game_folder(base.path()).join("Data").join("a.esp").exists());
    let installed = record(base.path());
    assert!(installed.files.is_empty());
    assert_eq!(installed.manifest_version, "");
    assert!(matches!(
        installer.plan(&v1, false),
        Err(InstallError::Cancelled)
    ));
}

#[tokio::test]
async fn the_hashed_runtime_files_are_checked_in_the_game_folder_through_the_cache() {
    let served = Served::new(&[("a.esp", b"first a")], &[]);
    let steam = fake_steam();
    let base = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (installer, _, _) = installer(steam.path(), base.path(), cache.path());
    let mut manifest = manifest_for(&served, "1", &[("Data/a.esp", "a.esp")]);
    let exe = steam.path().join("SkyrimSE.exe");
    manifest.runtime.files = vec![RuntimeFile {
        path: "SkyrimSE.exe".to_string(),
        size: fs::metadata(&exe).unwrap().len(),
        sha256: sha256_of_file(&exe).unwrap(),
    }];
    assert!(matches!(
        installer.check_runtime(&manifest, &mut Vec::new()),
        Err(InstallError::Runtime(RuntimeError::Missing { path })) if path == "SkyrimSE.exe"
    ));
    install(&installer, &manifest, false).await.unwrap();
    let mut notices = Vec::new();
    installer.check_runtime(&manifest, &mut notices).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let hashes = cache.path().join(SERVER_ID).join("hashes.json");
    let cached = fs::read_to_string(&hashes).unwrap();
    assert!(cached.contains("\"SkyrimSE.exe\""), "{cached}");
    // A cache file this launcher cannot read is set aside and said so; the check goes on with a fresh one.
    fs::write(&hashes, b"not json").unwrap();
    installer.check_runtime(&manifest, &mut notices).unwrap();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("hashes.json.bad"), "{}", notices[0]);
    assert!(
        cache
            .path()
            .join(SERVER_ID)
            .join("hashes.json.bad")
            .is_file()
    );
    assert!(
        fs::read_to_string(&hashes)
            .unwrap()
            .contains("\"SkyrimSE.exe\"")
    );
    // A changed exe in the game folder is noticed; the link is replaced so the stand-in Steam copy stays as it was.
    let linked = game_folder(base.path()).join("SkyrimSE.exe");
    fs::remove_file(&linked).unwrap();
    fs::write(&linked, b"exe byteS").unwrap();
    assert!(matches!(
        installer.check_runtime(&manifest, &mut Vec::new()),
        Err(InstallError::Runtime(RuntimeError::Hash { path })) if path == "SkyrimSE.exe"
    ));
    assert_eq!(fs::read(&exe).unwrap(), b"exe bytes");
    fs::write(&linked, b"short").unwrap();
    assert!(matches!(
        installer.check_runtime(&manifest, &mut Vec::new()),
        Err(InstallError::Runtime(RuntimeError::Size {
            expected: 9,
            actual: 5,
            ..
        }))
    ));
    // The next install links the vanilla file back, after which the check passes again.
    let report = install(&installer, &manifest, false).await.unwrap();
    assert_eq!((report.linked, report.kept), (1, 1));
    installer.check_runtime(&manifest, &mut Vec::new()).unwrap();
}

#[tokio::test]
async fn an_install_larger_than_the_free_space_is_refused_before_it_starts() {
    let served = Served::new(&[("a.esp", b"first a")], &[]);
    let steam = fake_steam();
    let base = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (installer, _, _) = installer(steam.path(), base.path(), cache.path());
    let mut v1 = manifest_for(&served, "1", &[("Data/a.esp", "a.esp")]);
    v1.files[0].size = u64::MAX / 2;
    let result = install(&installer, &v1, false).await;
    assert!(
        matches!(result, Err(InstallError::NoSpace { needed, .. }) if needed == u64::MAX / 2),
        "{result:?}"
    );
    assert!(!game_folder(base.path()).join("SkyrimSE.exe").exists());
}

/// Counts the regular files below a folder, the way the install walks the Steam copy.
fn count_files(folder: &Path) -> usize {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                count_files(&entry.path())
            } else {
                1
            }
        })
        .sum()
}
/// Removes the scratch folder however the test ends, so no link to the Steam copy is left behind.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The path as fsutil lists a link: the long name from the volume root, without the drive letter, lower-cased.
fn volume_relative(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap();
    let text = canonical.to_string_lossy().to_lowercase();
    let text = text.trim_start_matches(r"\\?\");
    text.split_once(':')
        .map_or(text.to_string(), |(_, rest)| rest.to_string())
}

#[tokio::test]
#[ignore = "needs the Steam copy of the game on this PC; run with --ignored and SUG_LAUNCHER_SCRATCH set to a folder on the same volume"]
async fn the_steam_copy_is_linked_and_never_changed() {
    let steam = game::find_skyrim().expect("Steam and the game must be installed");
    println!("Steam copy: {}", steam.display());
    let watched = [
        "SkyrimSE.exe",
        "Data/Skyrim.esm",
        "Data/Update.esm",
        "Data/Dawnguard.esm",
        "Data/HearthFires.esm",
        "Data/Dragonborn.esm",
    ];
    let hashes = || -> Vec<String> {
        watched
            .iter()
            .map(|rel| sha256_of_file(&steam.join(rel.replace('/', "\\"))).unwrap())
            .collect()
    };
    let before = hashes();
    for (rel, hash) in watched.iter().zip(&before) {
        println!("before  {hash}  {rel}");
    }
    let files_before = count_files(&steam);
    let scratch = Scratch(
        env::var_os("SUG_LAUNCHER_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir().join("sug-launcher-steam-test")),
    );
    let base = scratch.0.join("base");
    let cache = scratch.0.join("cache");
    fs::create_dir_all(&base).unwrap();
    let stand_in = b"a small file standing in for Skyrim.esm";
    let served = Served::new(&[("Skyrim.esm", stand_in)], &[]);
    let (installer, _, _) = installer(&steam, &base, &cache);
    let manifest = manifest_for(&served, "steam-test", &[("Data/Skyrim.esm", "Skyrim.esm")]);
    let report = install(&installer, &manifest, false).await.unwrap();
    println!("{report:?}");
    // The Steam copy is compared first, so a failing link check still says whether the copy changed.
    let after = hashes();
    for (rel, hash) in watched.iter().zip(&after) {
        println!("after   {hash}  {rel}");
    }
    assert_eq!(before, after);
    assert_eq!(count_files(&steam), files_before);
    assert_eq!((report.downloaded, report.copied), (1, 0));
    assert_eq!(report.linked, files_before - 1);
    let game_dir = game_folder(&base);
    assert_eq!(
        fs::read(game_dir.join("Data").join("Skyrim.esm")).unwrap(),
        stand_in
    );
    // fsutil lists every name of a file when asked from the Steam side; asked for a link under a user folder it may refuse.
    let exe = steam.join("SkyrimSE.exe");
    let listing = Command::new("fsutil")
        .args(["hardlink", "list"])
        .arg(&exe)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&listing.stdout).into_owned();
    println!("fsutil hardlink list {}:\n{text}", exe.display());
    let links: Vec<String> = text
        .lines()
        .map(|line| line.trim().to_lowercase())
        .filter(|line| !line.is_empty())
        .collect();
    let link = volume_relative(&game_dir.join("SkyrimSE.exe"));
    assert!(
        links.contains(&link),
        "{link} is not listed among the links of the Steam exe"
    );
    assert!(
        links.contains(&volume_relative(&exe)),
        "the Steam exe itself is not listed"
    );
    fs::remove_dir_all(&scratch.0).unwrap();
    assert_eq!(before, hashes());
    assert_eq!(count_files(&steam), files_before);
    println!(
        "scratch folder {} removed; the Steam copy hashes the same",
        scratch.0.display()
    );
}

// SPDX-License-Identifier: AGPL-3.0-or-later
//! One game run for a server: the load order swapped in, the launch file written, the loader started, and both put back at exit.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use serde::{Deserialize, Serialize};

use crate::config::write_atomic;
use crate::download::remove_if_present;
use crate::manifest::{PathError, relative_path};
use crate::secrets::{SecretError, write_protected};

pub const LAUNCH_FILE: &str = "launch.bin";
pub const LAUNCH_SCHEMA_VERSION: u32 = 1;
/// The variable through which the game client learns where the launch file is.
pub const ENV_LAUNCH_FILE: &str = "SUG_LAUNCH_FILE";
/// The loader's only argument: it then stays alive until the game exits, so its exit is the game's.
pub const WAIT_ARGUMENT: &str = "-waitforclose";
const BACKUP_SUFFIX: &str = ".backup";
/// The first line of every plugins.txt the launcher writes, so a leftover one is recognised and removed.
const MARKER: &str = "# A server's load order written by the launcher; the player's own file comes back when the game exits.";
const LINE_END: &str = "\r\n";

/// Where the game reads plugins.txt and where the launch file goes; handed in so tests never touch the real profile folder.
#[derive(Debug, Clone)]
pub struct Paths {
    pub plugins: PathBuf,
    pub launch_file: PathBuf,
}

impl Paths {
    /// The player's own plugins.txt while a game runs, next to the file itself.
    pub fn backup(&self) -> PathBuf {
        let mut name = self.plugins.file_name().unwrap_or_default().to_os_string();
        name.push(BACKUP_SUFFIX);
        self.plugins.with_file_name(name)
    }
}

/// What the game client needs to connect, as the backend answered Play; the client reads it under the same Windows user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchFile {
    schema_version: u32,
    pub host: String,
    pub port: u16,
    pub play_token: String,
    pub expires_at: u64,
}

impl LaunchFile {
    pub fn new(host: String, port: u16, play_token: String, expires_at: u64) -> Self {
        Self {
            schema_version: LAUNCH_SCHEMA_VERSION,
            host,
            port,
            play_token,
            expires_at,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("the loader path {path:?} is not allowed: {reason}")]
    LoaderPath { path: String, reason: PathError },
    #[error("the loader {path} is not in the game folder; install the server's files first")]
    LoaderMissing { path: String },
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("the launch file could not be encoded: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("the loader could not be started: {0}")]
    Spawn(io::Error),
    #[error("the loader could not be waited for: {0}")]
    Wait(io::Error),
}

fn io_at(path: &Path, source: io::Error) -> LaunchError {
    LaunchError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    }
}

/// What a start found left behind by the last run and put right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Recovered {
    pub plugins_restored: bool,
    pub plugins_removed: bool,
    pub launch_file_removed: bool,
}

/// At start: a run that ended through the launcher leaves nothing, so whatever is found is from a run that did not.
pub fn recover(paths: &Paths) -> Result<Recovered, LaunchError> {
    put_back(paths)
}

/// The text of plugins.txt for a load order: every plugin enabled, in the manifest's order, behind the marker line.
pub fn plugins_text(plugins: &[String]) -> String {
    let mut text = String::from(MARKER);
    text.push_str(LINE_END);
    for plugin in plugins {
        text.push('*');
        text.push_str(plugin);
        text.push_str(LINE_END);
    }
    text
}

/// True when the file at the path is one the launcher wrote; a missing file is not.
fn launcher_written(path: &Path) -> Result<bool, LaunchError> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes.starts_with(MARKER.as_bytes())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_at(path, error)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Restore {
    Restored,
    Removed,
    Nothing,
}

/// A backup goes back over plugins.txt; without one, a plugins.txt the launcher wrote is removed, since the player had none.
/// The backup carries the player's attributes with it, so a read-only file comes back read-only.
fn restore_plugins(paths: &Paths) -> Result<Restore, LaunchError> {
    let backup = paths.backup();
    if backup.is_file() {
        make_writable(&paths.plugins)?;
        fs::rename(&backup, &paths.plugins).map_err(|error| io_at(&backup, error))?;
        return Ok(Restore::Restored);
    }
    if launcher_written(&paths.plugins)? {
        fs::remove_file(&paths.plugins).map_err(|error| io_at(&paths.plugins, error))?;
        return Ok(Restore::Removed);
    }
    Ok(Restore::Nothing)
}

/// The player's file is copied aside once, attributes included; a backup already there is the older original and stays.
fn backup_plugins(paths: &Paths) -> Result<(), LaunchError> {
    let backup = paths.backup();
    if backup.exists() || launcher_written(&paths.plugins)? {
        return Ok(());
    }
    match fs::copy(&paths.plugins, &backup) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_at(&paths.plugins, error)),
    }
}

/// Clears the read-only attribute, which would refuse a rename over the file; a missing file needs nothing.
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "on Windows this clears one attribute; the lint is about Unix modes"
)]
fn make_writable(path: &Path) -> Result<(), LaunchError> {
    let mut permissions = match fs::metadata(path) {
        Ok(meta) => meta.permissions(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_at(path, error)),
    };
    if !permissions.readonly() {
        return Ok(());
    }
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).map_err(|error| io_at(path, error))
}

fn write_launch_file(paths: &Paths, launch: &LaunchFile) -> Result<(), LaunchError> {
    write_protected(&paths.launch_file, &serde_json::to_vec(launch)?)?;
    Ok(())
}

/// The player's file aside, the launch file written and the server's load order in place; the game can be started after this.
fn swap_in(paths: &Paths, plugins: &[String], launch: &LaunchFile) -> Result<(), LaunchError> {
    backup_plugins(paths)?;
    write_launch_file(paths, launch)?;
    make_writable(&paths.plugins)?;
    write_atomic(&paths.plugins, plugins_text(plugins).as_bytes())
        .map_err(|error| io_at(&paths.plugins, error))
}

/// The launch file goes first, so a client that reconnects after this point finds nothing to send, whatever the restore does.
fn put_back(paths: &Paths) -> Result<Recovered, LaunchError> {
    let launch_file_removed = paths.launch_file.exists();
    remove_if_present(&paths.launch_file).map_err(|error| io_at(&paths.launch_file, error))?;
    let restore = restore_plugins(paths)?;
    Ok(Recovered {
        plugins_restored: restore == Restore::Restored,
        plugins_removed: restore == Restore::Removed,
        launch_file_removed,
    })
}

/// Everything one start needs: the loader's path and the load order come from the server's manifest.
pub struct Start<'a> {
    pub paths: &'a Paths,
    pub game_folder: &'a Path,
    pub loader: &'a str,
    pub plugins: &'a [String],
    pub launch: &'a LaunchFile,
}

/// A running game: the loader process and the files that are put back when it exits.
/// Dropping it leaves the game running and the files in place; the next start puts them back.
pub struct Game {
    child: Child,
    paths: Paths,
    exit: Option<ExitStatus>,
}

impl Game {
    /// Backs up plugins.txt, writes the launch file and the load order, then starts the loader from the game folder.
    /// Whatever fails after the first write, the files are put back before the error is returned.
    pub fn start(start: &Start<'_>) -> Result<Self, LaunchError> {
        let loader = relative_path(start.loader).map_err(|reason| LaunchError::LoaderPath {
            path: start.loader.to_string(),
            reason,
        })?;
        let exe = start.game_folder.join(loader);
        if !exe.is_file() {
            return Err(LaunchError::LoaderMissing {
                path: start.loader.to_string(),
            });
        }
        let spawned = swap_in(start.paths, start.plugins, start.launch).and_then(|()| {
            Command::new(&exe)
                .current_dir(start.game_folder)
                .arg(WAIT_ARGUMENT)
                .env(ENV_LAUNCH_FILE, &start.paths.launch_file)
                .spawn()
                .map_err(LaunchError::Spawn)
        });
        match spawned {
            Ok(child) => Ok(Self {
                child,
                paths: start.paths.clone(),
                exit: None,
            }),
            Err(error) => {
                put_back(start.paths)?;
                Err(error)
            }
        }
    }

    /// Writes a new launch file for a Reconnect while the game runs; the client reads it at its next connect attempt.
    pub fn rewrite_launch_file(&self, launch: &LaunchFile) -> Result<(), LaunchError> {
        write_launch_file(&self.paths, launch)
    }

    /// Looks without waiting; the first look that finds the loader gone puts the files back.
    pub fn poll(&mut self) -> Result<Option<ExitStatus>, LaunchError> {
        if let Some(exit) = self.exit {
            return Ok(Some(exit));
        }
        match self.child.try_wait().map_err(LaunchError::Wait)? {
            Some(status) => self.finish(status).map(Some),
            None => Ok(None),
        }
    }

    /// Blocks until the loader exits, then puts the files back.
    pub fn wait(&mut self) -> Result<ExitStatus, LaunchError> {
        if let Some(exit) = self.exit {
            return Ok(exit);
        }
        let status = self.child.wait().map_err(LaunchError::Wait)?;
        self.finish(status)
    }

    /// The exit is recorded only once the files are back, so a failed restore is tried again at the next look.
    fn finish(&mut self, status: ExitStatus) -> Result<ExitStatus, LaunchError> {
        put_back(&self.paths)?;
        self.exit = Some(status);
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::fs::OpenOptionsExt;

    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    use super::*;
    use crate::secrets::read_protected;

    const ORIGINAL: &[u8] = b"*Mine.esp\r\n*Other.esp\r\n";

    fn paths(root: &Path) -> Paths {
        Paths {
            plugins: root.join("profile").join("plugins.txt"),
            launch_file: root.join("data").join(LAUNCH_FILE),
        }
    }

    fn set_read_only(path: &Path, read_only: bool) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(read_only);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn read_only(path: &Path) -> bool {
        fs::metadata(path).unwrap().permissions().readonly()
    }

    fn launch() -> LaunchFile {
        LaunchFile::new(
            "game.example.org".to_string(),
            7777,
            "sugp_TESTtestTESTtestTESTtestTESTtestTESTtes".to_string(),
            1_760_000_000_000,
        )
    }

    #[test]
    fn the_load_order_enables_every_plugin_in_order_behind_the_marker() {
        let text = plugins_text(&["Skyrim.esm".to_string(), "Example.esp".to_string()]);
        assert_eq!(text, format!("{MARKER}\r\n*Skyrim.esm\r\n*Example.esp\r\n"));
        assert_eq!(plugins_text(&[]), format!("{MARKER}\r\n"));
    }

    #[test]
    fn a_fresh_start_finds_nothing_to_put_back() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        assert_eq!(recover(&paths).unwrap(), Recovered::default());
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        assert_eq!(recover(&paths).unwrap(), Recovered::default());
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
    }

    #[test]
    fn plugins_are_restored_from_the_backup_after_a_crash_and_a_stale_launch_file_goes() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(paths.backup(), ORIGINAL).unwrap();
        fs::write(&paths.plugins, plugins_text(&["Server.esp".to_string()])).unwrap();
        write_launch_file(&paths, &launch()).unwrap();
        assert_eq!(
            recover(&paths).unwrap(),
            Recovered {
                plugins_restored: true,
                plugins_removed: false,
                launch_file_removed: true,
            }
        );
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
        assert!(!paths.backup().exists());
        assert!(!paths.launch_file.exists());
        assert_eq!(recover(&paths).unwrap(), Recovered::default());
    }

    #[test]
    fn a_launcher_written_plugins_file_without_a_backup_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, plugins_text(&["Server.esp".to_string()])).unwrap();
        assert_eq!(
            recover(&paths).unwrap(),
            Recovered {
                plugins_restored: false,
                plugins_removed: true,
                launch_file_removed: false,
            }
        );
        assert!(!paths.plugins.exists());
        // A file the player wrote never carries the marker, so it stays whatever else is found.
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        write_launch_file(&paths, &launch()).unwrap();
        assert_eq!(
            recover(&paths).unwrap(),
            Recovered {
                plugins_restored: false,
                plugins_removed: false,
                launch_file_removed: true,
            }
        );
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
    }

    #[test]
    fn the_backup_is_taken_once_and_never_of_the_launchers_own_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        backup_plugins(&paths).unwrap();
        assert!(!paths.backup().exists());
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, plugins_text(&[])).unwrap();
        backup_plugins(&paths).unwrap();
        assert!(!paths.backup().exists());
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        backup_plugins(&paths).unwrap();
        assert_eq!(fs::read(paths.backup()).unwrap(), ORIGINAL);
        fs::write(&paths.plugins, b"*Changed.esp\r\n").unwrap();
        backup_plugins(&paths).unwrap();
        assert_eq!(fs::read(paths.backup()).unwrap(), ORIGINAL);
    }

    #[test]
    fn the_launch_file_is_dpapi_over_the_play_answer() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let launch = launch();
        write_launch_file(&paths, &launch).unwrap();
        let stored = fs::read(&paths.launch_file).unwrap();
        assert!(
            !stored
                .windows(launch.play_token.len())
                .any(|w| w == launch.play_token.as_bytes())
        );
        let bytes = read_protected(&paths.launch_file).unwrap().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with(
            r#"{"schemaVersion":1,"host":"game.example.org","port":7777,"playToken":"sugp_"#
        ));
        assert_eq!(
            serde_json::from_slice::<LaunchFile>(&bytes).unwrap(),
            launch
        );
    }

    #[test]
    fn a_missing_or_unsafe_loader_is_refused_before_anything_is_written() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let game = root.path().join("game");
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        let launch = launch();
        let plugins = vec!["Server.esp".to_string()];
        let mut start = Start {
            paths: &paths,
            game_folder: &game,
            loader: "skse64_loader.exe",
            plugins: &plugins,
            launch: &launch,
        };
        assert!(matches!(
            Game::start(&start),
            Err(LaunchError::LoaderMissing { path }) if path == "skse64_loader.exe"
        ));
        start.loader = "../skse64_loader.exe";
        assert!(matches!(
            Game::start(&start),
            Err(LaunchError::LoaderPath {
                reason: PathError::DotSegment,
                ..
            })
        ));
        assert!(!paths.backup().exists());
        assert!(!paths.launch_file.exists());
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
    }

    #[test]
    fn a_loader_that_cannot_run_leaves_the_files_as_they_were() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let game = root.path().join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("loader.exe"), b"not a program").unwrap();
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        let launch = launch();
        let plugins = vec!["Server.esp".to_string()];
        let start = Start {
            paths: &paths,
            game_folder: &game,
            loader: "loader.exe",
            plugins: &plugins,
            launch: &launch,
        };
        assert!(matches!(Game::start(&start), Err(LaunchError::Spawn(_))));
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
        assert!(!paths.backup().exists());
        assert!(!paths.launch_file.exists());
    }

    #[test]
    fn a_plugins_file_that_cannot_be_replaced_fails_the_start_and_leaves_no_launch_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let game = root.path().join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("loader.exe"), b"not a program").unwrap();
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        // Another program holding the file without delete sharing refuses any rename over it.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&paths.plugins)
            .unwrap();
        let launch = launch();
        let plugins = vec!["Server.esp".to_string()];
        let start = Start {
            paths: &paths,
            game_folder: &game,
            loader: "loader.exe",
            plugins: &plugins,
            launch: &launch,
        };
        assert!(matches!(Game::start(&start), Err(LaunchError::Io { .. })));
        assert!(!paths.launch_file.exists());
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
        // The backup could not go back over the held file either, so it waits for the next start.
        assert_eq!(fs::read(paths.backup()).unwrap(), ORIGINAL);
        drop(held);
        assert!(recover(&paths).unwrap().plugins_restored);
        assert!(!paths.backup().exists());
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
    }

    #[test]
    fn a_read_only_plugins_file_is_replaced_for_the_run_and_comes_back_read_only() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        fs::create_dir_all(paths.plugins.parent().unwrap()).unwrap();
        fs::write(&paths.plugins, ORIGINAL).unwrap();
        set_read_only(&paths.plugins, true);
        let plugins = vec!["Server.esp".to_string()];
        swap_in(&paths, &plugins, &launch()).unwrap();
        // The attribute travels with the backup; the file the game reads must be writable for the restore.
        assert!(read_only(&paths.backup()));
        assert_eq!(fs::read(paths.backup()).unwrap(), ORIGINAL);
        assert!(!read_only(&paths.plugins));
        assert_eq!(
            fs::read_to_string(&paths.plugins).unwrap(),
            plugins_text(&plugins)
        );
        assert!(paths.launch_file.is_file());
        put_back(&paths).unwrap();
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
        assert!(read_only(&paths.plugins));
        assert!(!paths.backup().exists());
        assert!(!paths.launch_file.exists());
        // Left by a crash: a read-only file in the way does not block the restore, and the backup's attributes win.
        set_read_only(&paths.plugins, false);
        fs::write(paths.backup(), ORIGINAL).unwrap();
        fs::write(&paths.plugins, plugins_text(&plugins)).unwrap();
        set_read_only(&paths.plugins, true);
        assert_eq!(
            recover(&paths).unwrap(),
            Recovered {
                plugins_restored: true,
                plugins_removed: false,
                launch_file_removed: false,
            }
        );
        assert_eq!(fs::read(&paths.plugins).unwrap(), ORIGINAL);
        assert!(!read_only(&paths.plugins));
    }

    #[test]
    fn a_stale_launch_file_goes_even_when_the_plugins_file_cannot_be_restored() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        // A folder where the file should be: nothing can be renamed over it.
        fs::create_dir_all(&paths.plugins).unwrap();
        fs::write(paths.backup(), ORIGINAL).unwrap();
        write_launch_file(&paths, &launch()).unwrap();
        assert!(matches!(recover(&paths), Err(LaunchError::Io { .. })));
        assert!(!paths.launch_file.exists());
        assert_eq!(fs::read(paths.backup()).unwrap(), ORIGINAL);
    }
}

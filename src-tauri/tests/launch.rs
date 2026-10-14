// SPDX-License-Identifier: AGPL-3.0-or-later
//! A game run against a stub loader built here, since the real loader needs the game and a desktop.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use sug_launcher_lib::launch::{
    ENV_LAUNCH_FILE, Game, LAUNCH_FILE, LaunchFile, Paths, Start, plugins_text, recover,
};
use sug_launcher_lib::secrets::read_protected;
use tempfile::TempDir;

const ORIGINAL: &[u8] = b"*Mine.esp\r\n*Other.esp\r\n";
const LOADER: &str = "stub_loader.exe";
const OUTPUT: &str = "stub-loader.txt";
const STOP: &str = "stub-exit";
const PATIENCE: Duration = Duration::from_secs(20);

/// Writes where it runs, what the environment told it and its arguments, then waits for the stop file like a loader waits for the game.
const STUB: &str = r#"
use std::env;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let cwd = env::current_dir().expect("the current directory");
    let launch = env::var("SUG_LAUNCH_FILE").unwrap_or_default();
    let args: Vec<String> = env::args().skip(1).collect();
    let text = format!("cwd={}\nlaunch={}\nargs={}\n", cwd.display(), launch, args.join(" "));
    fs::write(cwd.join("stub-loader.txt"), text).expect("the output file");
    let stop = cwd.join("stub-exit");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !stop.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
}
"#;

/// A root with a game folder holding the stub loader, a profile folder and a data folder.
struct Fixture {
    /// Kept so the folders live as long as the fixture.
    _root: TempDir,
    paths: Paths,
    game: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let source = root.path().join("stub_loader.rs");
        fs::write(&source, STUB).unwrap();
        let rustc = Path::new(env!("CARGO")).with_file_name("rustc.exe");
        let built = Command::new(if rustc.is_file() {
            rustc.as_os_str().to_os_string()
        } else {
            "rustc".into()
        })
        .args(["--edition", "2024", "-o"])
        .arg(game.join(LOADER))
        .arg(&source)
        .output()
        .expect("rustc must be runnable to build the stub loader");
        assert!(
            built.status.success(),
            "rustc failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let paths = Paths {
            plugins: root.path().join("profile").join("plugins.txt"),
            launch_file: root.path().join("data").join(LAUNCH_FILE),
        };
        Self {
            _root: root,
            paths,
            game,
        }
    }

    fn with_original_plugins(self) -> Self {
        fs::create_dir_all(self.paths.plugins.parent().unwrap()).unwrap();
        fs::write(&self.paths.plugins, ORIGINAL).unwrap();
        self
    }

    fn stop_file(&self) -> PathBuf {
        self.game.join(STOP)
    }

    fn start(&self, plugins: &[String], launch: &LaunchFile) -> Game {
        Game::start(&Start {
            paths: &self.paths,
            game_folder: &self.game,
            loader: LOADER,
            plugins,
            launch,
        })
        .unwrap()
    }

    /// The stub's report, once it has written it.
    fn stub_output(&self) -> String {
        let path = self.game.join(OUTPUT);
        let deadline = Instant::now() + PATIENCE;
        while !path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        fs::read_to_string(&path).expect("the stub loader must write its report")
    }

    fn launch_file(&self) -> Option<LaunchFile> {
        read_protected(&self.paths.launch_file)
            .unwrap()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    /// Windows holds a running exe's image, so the stub is gone once its file can be removed.
    fn wait_for_stub_exit(&self) {
        let deadline = Instant::now() + PATIENCE;
        while fs::remove_file(self.game.join(LOADER)).is_err() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn launch(token: &str) -> LaunchFile {
    LaunchFile::new(
        "game.example.org".to_string(),
        7777,
        token.to_string(),
        1_760_000_000_000,
    )
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .to_lowercase()
}

fn set_read_only(path: &Path, read_only: bool) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(read_only);
    fs::set_permissions(path, permissions).unwrap();
}

fn read_only(path: &Path) -> bool {
    fs::metadata(path).unwrap().permissions().readonly()
}

#[test]
fn the_loader_runs_in_the_game_folder_with_the_launch_file_in_its_environment() {
    let fixture = Fixture::new().with_original_plugins();
    let plugins = vec!["Skyrim.esm".to_string(), "Example.esp".to_string()];
    let first = launch("sugp_FIRSTfirstFIRSTfirstFIRSTfirstFIRSTfir");
    let mut game = fixture.start(&plugins, &first);
    let report = fixture.stub_output();
    println!("{report}");
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(
        lines[0]
            .strip_prefix("cwd=")
            .map(|cwd| canonical(Path::new(cwd))),
        Some(canonical(&fixture.game))
    );
    // The stub reads the variable by its name, which is the whole contract with the game client.
    assert_eq!(ENV_LAUNCH_FILE, "SUG_LAUNCH_FILE");
    assert_eq!(
        lines[1],
        format!("launch={}", fixture.paths.launch_file.display())
    );
    assert_eq!(lines[2], "args=-waitforclose");
    assert!(
        game.poll().unwrap().is_none(),
        "the loader waits for the stop file"
    );
    // While the game runs: the server's load order is in place, the player's file is aside, the launch file holds the grant.
    assert_eq!(
        fs::read_to_string(&fixture.paths.plugins).unwrap(),
        plugins_text(&plugins)
    );
    assert_eq!(fs::read(fixture.paths.backup()).unwrap(), ORIGINAL);
    assert_eq!(fixture.launch_file(), Some(first.clone()));
    let second = launch("sugp_SECONDsecondSECONDsecondSECONDsecondSE");
    game.rewrite_launch_file(&second).unwrap();
    assert_eq!(fixture.launch_file(), Some(second));
    assert!(game.poll().unwrap().is_none());
    fs::write(fixture.stop_file(), b"").unwrap();
    let exit = game.wait().unwrap();
    assert!(exit.success(), "{exit}");
    assert!(!fixture.paths.launch_file.exists());
    assert_eq!(fs::read(&fixture.paths.plugins).unwrap(), ORIGINAL);
    assert!(!fixture.paths.backup().exists());
    assert_eq!(game.poll().unwrap(), Some(exit));
    assert_eq!(game.wait().unwrap(), exit);
    assert!(!fixture.paths.launch_file.exists());
}

#[test]
fn without_a_plugins_file_the_written_one_is_removed_at_exit() {
    let fixture = Fixture::new();
    fs::write(fixture.stop_file(), b"").unwrap();
    let plugins = vec!["Example.esp".to_string()];
    let mut game = fixture.start(
        &plugins,
        &launch("sugp_TESTtestTESTtestTESTtestTESTtestTESTtes"),
    );
    assert!(fixture.paths.plugins.is_file());
    assert!(!fixture.paths.backup().exists());
    let exit = game.wait().unwrap();
    assert!(exit.success(), "{exit}");
    assert!(!fixture.paths.plugins.exists());
    assert!(!fixture.paths.backup().exists());
    assert!(!fixture.paths.launch_file.exists());
    assert_eq!(
        fixture.stub_output().lines().nth(1),
        Some(format!("launch={}", fixture.paths.launch_file.display()).as_str())
    );
}

#[test]
fn a_read_only_plugins_file_is_swapped_for_the_run_and_comes_back_read_only() {
    let fixture = Fixture::new().with_original_plugins();
    set_read_only(&fixture.paths.plugins, true);
    fs::write(fixture.stop_file(), b"").unwrap();
    let plugins = vec!["Example.esp".to_string()];
    let mut game = fixture.start(
        &plugins,
        &launch("sugp_TESTtestTESTtestTESTtestTESTtestTESTtes"),
    );
    assert!(read_only(&fixture.paths.backup()));
    assert!(!read_only(&fixture.paths.plugins));
    assert_eq!(
        fs::read_to_string(&fixture.paths.plugins).unwrap(),
        plugins_text(&plugins)
    );
    let exit = game.wait().unwrap();
    assert!(exit.success(), "{exit}");
    assert_eq!(fs::read(&fixture.paths.plugins).unwrap(), ORIGINAL);
    assert!(read_only(&fixture.paths.plugins));
    assert!(!fixture.paths.backup().exists());
    assert!(!fixture.paths.launch_file.exists());
    set_read_only(&fixture.paths.plugins, false);
}

#[test]
fn a_launcher_that_dies_during_a_run_leaves_files_the_next_start_puts_back() {
    let fixture = Fixture::new().with_original_plugins();
    fs::write(fixture.stop_file(), b"").unwrap();
    let plugins = vec!["Example.esp".to_string()];
    let game = fixture.start(
        &plugins,
        &launch("sugp_TESTtestTESTtestTESTtestTESTtestTESTtes"),
    );
    fixture.stub_output();
    // The launcher goes away without ever looking at the loader again.
    drop(game);
    fixture.wait_for_stub_exit();
    assert_eq!(
        fs::read_to_string(&fixture.paths.plugins).unwrap(),
        plugins_text(&plugins)
    );
    assert_eq!(fs::read(fixture.paths.backup()).unwrap(), ORIGINAL);
    assert!(fixture.launch_file().is_some());
    let recovered = recover(&fixture.paths).unwrap();
    println!("{recovered:?}");
    assert!(recovered.plugins_restored);
    assert!(!recovered.plugins_removed);
    assert!(recovered.launch_file_removed);
    assert_eq!(fs::read(&fixture.paths.plugins).unwrap(), ORIGINAL);
    assert!(!fixture.paths.backup().exists());
    assert!(!fixture.paths.launch_file.exists());
}

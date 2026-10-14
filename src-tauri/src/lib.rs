// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Tauri shell: plugins, managed state and the commands the UI calls.

pub mod api;
pub mod config;
pub mod deploy;
pub mod download;
pub mod game;
pub mod launch;
pub mod log;
pub mod manifest;
pub mod play;
pub mod secrets;
pub mod servers;
pub mod signin;
#[cfg(test)]
mod test_support;
pub mod update;

use std::collections::HashMap;
use std::fmt::Display;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use minisign_verify::PublicKey;
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use url::Url;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::HSTRING;

use api::{Access, ApiClient, ApiError, PlayGrant, ServerCheck, ServerInfo, Status};
use config::{AppDirs, ConfigError, LauncherConfig, SettingsError, SettingsStore};
use deploy::{Folders, InstallError, Installer, Phase, ProgressHandle, Report};
use download::{Cancel, DownloadError, Downloader};
use launch::{Game, LaunchFile, Start};
use log::Logger;
use manifest::{Context, Manifest};
use play::{Effect, Event, Machine, Notice};
use secrets::{Session, SessionStore};
use servers::{DirectoryReport, ServerEntry, ServerStore, StoreError};
use signin::Flow;
use update::{Check, Progress};

/// How often a running game is looked at for its exit.
const GAME_POLL: Duration = Duration::from_millis(500);

/// Why the launcher cannot start at all; anything else that fails at start is recovered from and shown in the list.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("the app folders could not be found: {0}")]
    Dirs(tauri::Error),
    #[error("the app folders could not be created: {0}")]
    CreateDirs(io::Error),
    #[error("the log could not be opened: {0}")]
    Log(io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Client(#[from] ApiError),
    #[error(transparent)]
    Downloader(#[from] DownloadError),
    #[error(transparent)]
    Servers(#[from] StoreError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

/// What the last refresh found, so returning to the list shows it again without another fetch.
#[derive(Default)]
struct LastRefresh {
    directory: Option<DirectoryReport>,
    checks: HashMap<String, ServerCheck>,
}

/// The one install that may run at a time; its outcome stays until the UI has read it.
struct RunningInstall {
    id: String,
    cancel: Cancel,
    progress: ProgressHandle,
    outcome: Arc<Mutex<Option<Result<Report, InstallError>>>>,
}

/// How a sign-in ended, left for the UI to read: the display name, or why it failed.
type SignInOutcome = Arc<Mutex<Option<Result<String, String>>>>;

/// The one sign-in that may wait for the browser at a time; its outcome stays until the UI has read it.
struct RunningSignIn {
    id: String,
    /// Absent while the start request is still on its way; the task waits for the browser once it is set.
    task: Option<JoinHandle<()>>,
    outcome: SignInOutcome,
}

/// One game run at a time: the machine, the server it is for and the loader process while it lives.
#[derive(Default)]
struct PlayRun {
    machine: Machine,
    id: Option<String>,
    game: Option<Game>,
}

pub struct AppState {
    config: LauncherConfig,
    signing_key: Option<PublicKey>,
    log: Arc<Logger>,
    client: ApiClient,
    downloader: Downloader,
    dirs: AppDirs,
    product: String,
    user_agent: String,
    sessions: SessionStore,
    servers: Mutex<ServerStore>,
    settings: Mutex<SettingsStore>,
    refresh: Mutex<LastRefresh>,
    update: Arc<update::Slot>,
    install: Mutex<Option<RunningInstall>>,
    signin: Mutex<Option<RunningSignIn>>,
    play: Arc<Mutex<PlayRun>>,
}

impl AppState {
    fn build(app: &AppHandle) -> Result<Self, StartError> {
        let dirs = AppDirs::resolve(app).map_err(StartError::Dirs)?;
        dirs.create().map_err(StartError::CreateDirs)?;
        let log = Arc::new(Logger::open(&dirs.log).map_err(StartError::Log)?);
        recover_last_run(&dirs, &log);
        let config = LauncherConfig::bundled()?;
        let signing_key = config::signing_key(&app.config().plugins.0)?;
        let version = app.package_info().version.to_string();
        let product = app
            .config()
            .product_name
            .clone()
            .unwrap_or_else(|| "launcher".to_string());
        let user_agent = format!("{}/{version}", product.replace(' ', ""));
        let client = ApiClient::new(&user_agent)?;
        let downloader = Downloader::new(&user_agent)?;
        let servers = ServerStore::load(&dirs.config, &dirs.cache)?;
        let settings = SettingsStore::load(&dirs.config)?;
        let sessions = SessionStore::new(&dirs.data);
        log.info(&format!("launcher {version} started"));
        if signing_key.is_none() {
            log.warn("no signing key in tauri.conf.json, so neither updates nor the directory are fetched");
        }
        Ok(Self {
            config,
            signing_key,
            log,
            client,
            downloader,
            dirs,
            product,
            user_agent,
            sessions,
            servers: Mutex::new(servers),
            settings: Mutex::new(settings),
            refresh: Mutex::new(LastRefresh::default()),
            update: Arc::new(update::Slot::default()),
            install: Mutex::new(None),
            signin: Mutex::new(None),
            play: Arc::new(Mutex::new(PlayRun::default())),
        })
    }
}

/// Where the game reads plugins.txt and where the launch file for the game client goes.
fn launch_paths(dirs: &AppDirs) -> launch::Paths {
    launch::Paths {
        plugins: game::plugins_path(&dirs.local_data),
        launch_file: dirs.data.join(launch::LAUNCH_FILE),
    }
}

/// A game run that did not end through the launcher leaves its files behind; they are put back before anything else.
fn recover_last_run(dirs: &AppDirs, log: &Logger) {
    match launch::recover(&launch_paths(dirs)) {
        Ok(recovered) => {
            if recovered.plugins_restored {
                log.info("plugins.txt put back from the backup of the last game run");
            }
            if recovered.plugins_removed {
                log.info("the plugins.txt of the last game run removed; the player had none");
            }
            if recovered.launch_file_removed {
                log.info("a leftover launch file removed");
            }
        }
        Err(error) => log.warn(&format!(
            "the last game run could not be put right: {error}"
        )),
    }
}

/// What the UI shows when a command fails; the text is already fit for a player.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiError {
    message: String,
}

impl<E: Display> From<E> for UiError {
    fn from(error: E) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherInfo {
    version: String,
    website: String,
    rules_url: String,
}

/// One list row; `check` is what the last refresh learned from the server, absent when it did not answer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRow {
    entry: ServerEntry,
    check: Option<ServerCheck>,
}

/// The list; `set_aside` names files the launcher could not read and renamed, reported once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerList {
    servers: Vec<ServerRow>,
    directory: Option<DirectoryReport>,
    set_aside: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRow {
    id: String,
    status: Option<Status>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPage {
    entry: ServerEntry,
    check: ServerCheck,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPreview {
    origin: String,
    name: String,
    description: String,
    website: String,
    warning: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    base_folder: Option<String>,
    default_base_folder: Option<String>,
}

/// A file that runs as native code, with the origin it is fetched from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFile {
    to: String,
    origin: String,
}

/// The install state of a server's page: why it cannot be installed, or what an install would do.
#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InstallState {
    Unavailable {
        reason: String,
    },
    Refused {
        reason: String,
    },
    Ready {
        version: String,
        installed_version: Option<String>,
        native: Vec<NativeFile>,
        to_download: usize,
        bytes: u64,
        to_remove: usize,
        /// Files of the Steam copy not yet linked or copied into the game folder.
        to_place: usize,
        game_folder: String,
    },
}

/// What the running install is doing, or how the last one ended; an ending is reported once.
#[derive(Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InstallProgress {
    Idle,
    Running { id: String, phase: Phase },
    Done { id: String, report: Report },
    Failed { id: String, message: String },
    Cancelled { id: String },
}

/// The sign-in a server's page shows; `banned` means the stored sign-in is already gone.
#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SignInView {
    SignedOut,
    SignedIn {
        display_name: String,
        access: Access,
    },
    Banned {
        display_name: String,
        ban_until: Option<u64>,
        ban_reason: Option<String>,
    },
    /// A sign-in is stored, but the server could not say whether it still allows play.
    Unchecked {
        display_name: String,
        reason: String,
    },
}

/// Where the one sign-in stands; an ending is reported once.
#[derive(Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SignInProgress {
    Idle,
    Waiting { id: String },
    Done { id: String, display_name: String },
    Failed { id: String, message: String },
}

/// Where the one game run stands, for the server it is for.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayView {
    state: play::State,
    id: Option<String>,
    notice: Option<Notice>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn settings_view(store: &SettingsStore, product: &str) -> SettingsView {
    SettingsView {
        base_folder: store.settings().base_folder.clone(),
        default_base_folder: game::find_skyrim()
            .ok()
            .and_then(|game| config::default_base_folder(&game, product))
            .map(|path| path.to_string_lossy().into_owned()),
    }
}

fn server_list(servers: &ServerStore, refresh: &LastRefresh, set_aside: Vec<String>) -> ServerList {
    ServerList {
        servers: servers
            .entries()
            .iter()
            .map(|entry| ServerRow {
                entry: entry.clone(),
                check: refresh.checks.get(&entry.id).cloned(),
            })
            .collect(),
        directory: refresh.directory.clone(),
        set_aside,
    }
}

fn entry_of(state: &AppState, id: &str) -> Result<ServerEntry, UiError> {
    lock(&state.servers)
        .get(id)
        .cloned()
        .ok_or_else(|| UiError::from("unknown server"))
}

/// Runs one request per entry at the same time and pairs every answer with its entry's id.
async fn per_entry<T, Fut>(
    entries: Vec<ServerEntry>,
    request: impl Fn(ServerEntry) -> Fut,
) -> tauri::Result<Vec<(String, T)>>
where
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let tasks: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            (
                entry.id.clone(),
                tauri::async_runtime::spawn(request(entry)),
            )
        })
        .collect();
    let mut answers = Vec::with_capacity(tasks.len());
    for (id, task) in tasks {
        answers.push((id, task.await?));
    }
    Ok(answers)
}

/// The server info of an origin the launcher can use; any other API version is an error with its own text.
async fn usable_server_info(state: &AppState, origin: &Url) -> Result<ServerInfo, UiError> {
    match state.client.server(origin).await? {
        ServerCheck::Ok { info } => Ok(info),
        ServerCheck::LauncherTooOld { api_version } => Err(UiError::from(format!(
            "this server needs a newer launcher (it speaks API version {api_version})"
        ))),
        ServerCheck::ServerTooOld { api_version } => Err(UiError::from(format!(
            "this server is too old for this launcher (it speaks API version {api_version})"
        ))),
    }
}

/// Everything an install of one server needs, fetched fresh: the Steam copy, the manifest and the folders.
struct ServerContext {
    origin: Url,
    steam: PathBuf,
    manifest: Manifest,
    folders: Folders,
}

/// Unavailable: the game, the server or the manifest could not be reached. Refused: the manifest broke a rule.
enum ContextError {
    Unavailable(String),
    Refused(String),
}

impl ContextError {
    fn reason(self) -> String {
        match self {
            ContextError::Unavailable(reason) | ContextError::Refused(reason) => reason,
        }
    }
}

async fn server_context(state: &AppState, id: &str) -> Result<ServerContext, ContextError> {
    let entry = entry_of(state, id).map_err(|error| ContextError::Unavailable(error.message))?;
    let info = usable_server_info(state, &entry.backend_url)
        .await
        .map_err(|error| ContextError::Unavailable(error.message))?;
    let steam =
        game::find_skyrim().map_err(|error| ContextError::Unavailable(error.to_string()))?;
    let build = game::file_version(&steam.join(game::EXE))
        .map_err(|error| ContextError::Unavailable(error.to_string()))?;
    game::check_build(&build, &info.game_runtime)
        .map_err(|error| ContextError::Unavailable(error.to_string()))?;
    let steam_data =
        game::data_files(&steam).map_err(|error| ContextError::Unavailable(error.to_string()))?;
    let bytes = state
        .client
        .manifest(&entry.backend_url, &info.manifest_url)
        .await
        .map_err(|error| ContextError::Unavailable(format!("the manifest: {error}")))?;
    let manifest = manifest::parse(
        &bytes,
        &Context {
            game_runtime: &info.game_runtime,
            steam_data: &steam_data,
        },
    )
    .map_err(|error| ContextError::Refused(error.to_string()))?;
    let base = match &lock(&state.settings).settings().base_folder {
        Some(folder) => PathBuf::from(folder),
        None => config::default_base_folder(&steam, &state.product).ok_or_else(|| {
            ContextError::Unavailable(
                "no base folder could be chosen; set one in Settings".to_string(),
            )
        })?,
    };
    let folders = Folders::new(&base, &state.dirs.cache, &entry.id);
    if deploy::overlapping(&folders.game, &steam) {
        return Err(ContextError::Unavailable(
            "the server's game folder would sit inside the Steam copy of the game or contain it; choose another base folder in Settings".to_string(),
        ));
    }
    Ok(ServerContext {
        origin: entry.backend_url,
        steam,
        manifest,
        folders,
    })
}

fn native_files(manifest: &Manifest) -> Vec<NativeFile> {
    manifest
        .native_files()
        .map(|file| NativeFile {
            to: file.to.clone(),
            origin: file.source.origin(),
        })
        .collect()
}

/// The stored sign-in for a server, or None; a file this user cannot read is useless and goes.
fn load_session(state: &AppState, id: &str) -> Option<Session> {
    match state.sessions.load(id, config::unix_now_ms()) {
        Ok(session) => session,
        Err(error) => {
            state
                .log
                .warn(&format!("the stored sign-in for {id} is unusable: {error}"));
            forget_session(state, id, "unusable");
            None
        }
    }
}

fn forget_session(state: &AppState, id: &str, why: &str) {
    match state.sessions.delete(id) {
        Ok(true) => state.log.info(&format!("sign-in for {id} removed: {why}")),
        Ok(false) => {}
        Err(error) => state.log.warn(&format!(
            "the sign-in for {id} could not be removed: {error}"
        )),
    }
}

/// Takes the sign-in that owns `outcome` out of its slot and stops its task; a cancel may have taken it already.
fn end_sign_in(state: &AppState, outcome: &SignInOutcome) {
    let mut slot = lock(&state.signin);
    let own = slot
        .as_ref()
        .is_some_and(|running| Arc::ptr_eq(&running.outcome, outcome));
    if own && let Some(task) = slot.take().and_then(|running| running.task) {
        task.abort();
    }
}

#[tauri::command]
fn launcher_info(app: AppHandle, state: State<'_, AppState>) -> LauncherInfo {
    LauncherInfo {
        version: app.package_info().version.to_string(),
        website: state.config.website.to_string(),
        rules_url: state.config.rules_url.to_string(),
    }
}

/// The check at start; the UI runs nothing else until an available update is installed or has failed.
#[tauri::command]
async fn update_check(app: AppHandle, state: State<'_, AppState>) -> Result<Check, UiError> {
    let check = state.update.check(&app, &state.user_agent).await;
    match &check {
        Check::UpToDate => state.log.info("no update: this build is current"),
        Check::Available { version } => state.log.info(&format!(
            "update to {version} available; installing it first"
        )),
        Check::Skipped { reason } => state.log.warn(&format!("update check skipped: {reason}")),
    }
    Ok(check)
}

/// Downloads and installs the pending update in the background; once the installer runs, the launcher exits.
#[tauri::command]
fn update_install(state: State<'_, AppState>) {
    let slot = state.update.clone();
    let log = state.log.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = match slot.download().await {
            Ok(bytes) => {
                log.info(&format!(
                    "update downloaded and verified, {} bytes",
                    bytes.len()
                ));
                slot.install(&bytes)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            log.warn(&format!("update failed, going on with this build: {error}"));
        }
    });
}

#[tauri::command]
fn update_progress(state: State<'_, AppState>) -> Progress {
    state.update.progress()
}

/// Refreshes the directory and asks every server for its API version; the UI calls this at the start and on Refresh.
#[tauri::command]
async fn load_servers(state: State<'_, AppState>) -> Result<ServerList, UiError> {
    let now = config::unix_now();
    let (directory, delisted) = match &state.signing_key {
        None => (DirectoryReport::Unconfigured, Vec::new()),
        Some(key) => {
            let fetched =
                servers::fetch_directory(&state.client, &state.config.directory_url, key).await;
            match lock(&state.servers).take_directory(key, fetched, now) {
                Ok(applied) => applied,
                Err(error) => (
                    DirectoryReport::Unavailable {
                        reason: error.to_string(),
                    },
                    Vec::new(),
                ),
            }
        }
    };
    match &directory {
        DirectoryReport::Fresh => state.log.info("directory refreshed"),
        DirectoryReport::Cached { reason } => state
            .log
            .warn(&format!("directory kept from the last good copy: {reason}")),
        DirectoryReport::Unavailable { reason } => {
            state.log.warn(&format!("directory unavailable: {reason}"));
        }
        DirectoryReport::Unconfigured => {}
    }
    // A server that left the directory left the list, and its sign-in goes with the entry.
    for id in delisted {
        state
            .log
            .info(&format!("server {id} is no longer on the directory"));
        forget_session(&state, &id, "no longer listed");
    }
    let entries = lock(&state.servers).entries().to_vec();
    let client = state.client.clone();
    let answers = per_entry(entries, move |entry| {
        let client = client.clone();
        async move { client.server(&entry.backend_url).await }
    })
    .await?;
    let mut checks = HashMap::with_capacity(answers.len());
    for (id, answer) in answers {
        match answer {
            Ok(check) => {
                checks.insert(id, check);
            }
            Err(error) => state.log.warn(&format!("server {id}: {error}")),
        }
    }
    let set_aside: Vec<String> = lock(&state.servers)
        .take_set_aside()
        .into_iter()
        .chain(lock(&state.settings).take_set_aside())
        .collect();
    for reason in &set_aside {
        state.log.warn(reason);
    }
    let mut refresh = lock(&state.refresh);
    refresh.directory = Some(directory);
    refresh.checks = checks;
    Ok(server_list(&lock(&state.servers), &refresh, set_aside))
}

/// The list as the last refresh left it, for returning to the list screen.
#[tauri::command]
fn list_servers(state: State<'_, AppState>) -> ServerList {
    server_list(&lock(&state.servers), &lock(&state.refresh), Vec::new())
}

#[tauri::command]
async fn server_status(state: State<'_, AppState>) -> Result<Vec<StatusRow>, UiError> {
    let entries = lock(&state.servers).entries().to_vec();
    let client = state.client.clone();
    let answers = per_entry(entries, move |entry| {
        let client = client.clone();
        async move { client.status(&entry.backend_url).await }
    })
    .await?;
    let mut rows = Vec::with_capacity(answers.len());
    for (id, status) in answers {
        let status = match status {
            Ok(status) => Some(status),
            Err(error) => {
                state.log.warn(&format!("status of {id}: {error}"));
                None
            }
        };
        rows.push(StatusRow { id, status });
    }
    Ok(rows)
}

#[tauri::command]
async fn server_page(id: String, state: State<'_, AppState>) -> Result<ServerPage, UiError> {
    let entry = entry_of(&state, &id)?;
    let check = match state.client.server(&entry.backend_url).await {
        Ok(check) => check,
        Err(error) => {
            state.log.warn(&format!("server {}: {error}", entry.id));
            return Err(error.into());
        }
    };
    Ok(ServerPage { entry, check })
}

/// Fetches the manifest and plans an install without running it, so the page shows what Install would do.
#[tauri::command]
async fn install_state(id: String, state: State<'_, AppState>) -> Result<InstallState, UiError> {
    let context = match server_context(&state, &id).await {
        Ok(context) => context,
        Err(ContextError::Unavailable(reason)) => {
            state.log.warn(&format!("install state of {id}: {reason}"));
            return Ok(InstallState::Unavailable { reason });
        }
        Err(ContextError::Refused(reason)) => {
            state
                .log
                .warn(&format!("manifest of {id} refused: {reason}"));
            return Ok(InstallState::Refused { reason });
        }
    };
    let native = native_files(&context.manifest);
    let version = context.manifest.version.clone();
    let game_folder = context.folders.game.to_string_lossy().into_owned();
    let installer = Installer::new(
        context.steam,
        context.folders,
        Cancel::default(),
        Arc::new(Mutex::new(Phase::Planning)),
    );
    let manifest = context.manifest;
    let plan =
        tauri::async_runtime::spawn_blocking(move || installer.plan(&manifest, false)).await?;
    let plan = match plan {
        Ok(plan) => plan,
        Err(error @ (InstallError::Archive { .. } | InstallError::Path { .. })) => {
            return Ok(InstallState::Refused {
                reason: error.to_string(),
            });
        }
        Err(error) => {
            return Ok(InstallState::Unavailable {
                reason: error.to_string(),
            });
        }
    };
    Ok(InstallState::Ready {
        version,
        installed_version: plan.installed_version().map(String::from),
        native,
        to_download: plan.download.len(),
        bytes: plan.bytes,
        to_remove: plan.remove.len(),
        to_place: plan.runtime.iter().filter(|file| !file.present).count(),
        game_folder,
    })
}

/// Starts an install in the background; `verify` hashes every file in full instead of trusting the cache.
#[tauri::command]
async fn install_start(
    id: String,
    verify: bool,
    state: State<'_, AppState>,
) -> Result<(), UiError> {
    let cancel = Cancel::default();
    let progress: ProgressHandle = Arc::new(Mutex::new(Phase::Planning));
    let outcome = Arc::new(Mutex::new(None));
    // The slot is taken before the first await, so a second click while the manifest is fetched is refused.
    {
        let mut slot = lock(&state.install);
        if slot
            .as_ref()
            .is_some_and(|running| lock(&running.outcome).is_none())
        {
            return Err(UiError::from("an install is already running"));
        }
        *slot = Some(RunningInstall {
            id: id.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
            outcome: outcome.clone(),
        });
    }
    let context = match server_context(&state, &id).await {
        Ok(context) => context,
        Err(error) => {
            *lock(&state.install) = None;
            return Err(UiError::from(error.reason()));
        }
    };
    state
        .log
        .info(&format!("install of {id} started, verify {verify}"));
    let installer = Installer::new(context.steam, context.folders, cancel, progress);
    let manifest = context.manifest;
    let downloader = state.downloader.clone();
    let log = state.log.clone();
    // Hashing and linking block, so the whole install runs on a blocking thread that drives its downloads itself.
    tauri::async_runtime::spawn_blocking(move || {
        let result = installer.plan(&manifest, verify).and_then(|plan| {
            tauri::async_runtime::block_on(installer.run(&manifest, plan, &downloader))
        });
        match &result {
            Ok(report) => log.info(&format!(
                "install of {id} finished: {} downloaded, {} kept, {} removed, {} linked, {} copied",
                report.downloaded, report.kept, report.removed, report.linked, report.copied
            )),
            Err(InstallError::Cancelled) => log.info(&format!("install of {id} cancelled")),
            Err(error) => log.warn(&format!("install of {id} failed: {error}")),
        }
        *lock(&outcome) = Some(result);
    });
    Ok(())
}

#[tauri::command]
fn install_progress(state: State<'_, AppState>) -> InstallProgress {
    let mut slot = lock(&state.install);
    let Some(running) = slot.as_ref() else {
        return InstallProgress::Idle;
    };
    let id = running.id.clone();
    let Some(result) = lock(&running.outcome).take() else {
        return InstallProgress::Running {
            id,
            phase: lock(&running.progress).clone(),
        };
    };
    *slot = None;
    match result {
        Ok(report) => InstallProgress::Done { id, report },
        Err(InstallError::Cancelled) => InstallProgress::Cancelled { id },
        Err(error) => InstallProgress::Failed {
            id,
            message: error.to_string(),
        },
    }
}

#[tauri::command]
fn install_cancel(state: State<'_, AppState>) {
    if let Some(running) = lock(&state.install).as_ref() {
        running.cancel.cancel();
    }
}

#[tauri::command]
async fn preview_server(url: String, state: State<'_, AppState>) -> Result<AddPreview, UiError> {
    let origin = servers::validate_origin(&url)?;
    let info = usable_server_info(&state, &origin).await?;
    Ok(AddPreview {
        origin: origin.to_string(),
        name: info.name,
        description: info.description,
        website: info.website,
        warning: servers::UNLISTED_WARNING,
    })
}

#[tauri::command]
async fn add_server(url: String, state: State<'_, AppState>) -> Result<ServerEntry, UiError> {
    let origin = servers::validate_origin(&url)?;
    let info = usable_server_info(&state, &origin).await?;
    let entry = lock(&state.servers).add_manual(origin, info.name, config::unix_now())?;
    state.log.info(&format!("server {} added by URL", entry.id));
    Ok(entry)
}

/// Removes the entry and the sign-in stored for it; the server's game folder stays for the player to delete.
#[tauri::command]
fn remove_server(id: String, state: State<'_, AppState>) -> Result<(), UiError> {
    lock(&state.servers).remove(&id)?;
    lock(&state.refresh).checks.remove(&id);
    state.sessions.delete(&id)?;
    state.log.info(&format!("server {id} removed"));
    Ok(())
}

#[tauri::command]
fn open_link(url: String, app: AppHandle) -> Result<(), UiError> {
    let link = Url::parse(&url).map_err(|_| UiError::from("not a valid link"))?;
    if !matches!(link.scheme(), "http" | "https") {
        return Err(UiError::from("only http and https links can be opened"));
    }
    app.opener().open_url(link.as_str(), None::<&str>)?;
    Ok(())
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> SettingsView {
    settings_view(&lock(&state.settings), &state.product)
}

#[tauri::command]
fn set_base_folder(folder: String, state: State<'_, AppState>) -> Result<SettingsView, UiError> {
    let mut settings = lock(&state.settings);
    settings.set_base_folder(&folder)?;
    state.log.info("base folder changed");
    Ok(settings_view(&settings, &state.product))
}

/// Asks the server whether the stored sign-in still allows play; a sign-in it no longer knows, or a ban, removes the file.
#[tauri::command]
async fn signin_state(id: String, state: State<'_, AppState>) -> Result<SignInView, UiError> {
    let entry = entry_of(&state, &id)?;
    let Some(session) = load_session(&state, &id) else {
        return Ok(SignInView::SignedOut);
    };
    match state.client.me(&entry.backend_url, &session.session).await {
        Ok(me) => Ok(match me.access {
            Access::Banned => {
                forget_session(&state, &id, "banned");
                SignInView::Banned {
                    display_name: me.display_name,
                    ban_until: me.ban_until,
                    ban_reason: me.ban_reason,
                }
            }
            access => SignInView::SignedIn {
                display_name: me.display_name,
                access,
            },
        }),
        Err(ApiError::Unauthorized) => {
            forget_session(&state, &id, "no longer valid");
            Ok(SignInView::SignedOut)
        }
        Err(error) => {
            state.log.warn(&format!(
                "the sign-in for {id} could not be checked: {error}"
            ));
            Ok(SignInView::Unchecked {
                display_name: session.display_name,
                reason: error.to_string(),
            })
        }
    }
}

/// Starts a sign-in and opens the browser; the rest runs in the background until the browser comes back.
#[tauri::command]
async fn signin_start(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), UiError> {
    let entry = entry_of(&state, &id)?;
    let outcome: SignInOutcome = Arc::new(Mutex::new(None));
    // The slot is taken before the first await, so a second click while the server is asked is refused.
    {
        let mut slot = lock(&state.signin);
        if slot
            .as_ref()
            .is_some_and(|running| lock(&running.outcome).is_none())
        {
            return Err(UiError::from(
                "a sign-in is already waiting for the browser; finish or cancel it first",
            ));
        }
        *slot = Some(RunningSignIn {
            id: id.clone(),
            task: None,
            outcome: outcome.clone(),
        });
    }
    let flow = match Flow::start(&state.client, &entry.backend_url).await {
        Ok(flow) => flow,
        Err(error) => {
            end_sign_in(&state, &outcome);
            state
                .log
                .warn(&format!("sign-in to {id} could not start: {error}"));
            return Err(error.into());
        }
    };
    let authorize_url = flow.authorize_url().clone();
    let port = flow.port();
    // A cancel while the server was asked emptied the slot; the flow then goes with its listener and no browser opens.
    {
        let mut slot = lock(&state.signin);
        let Some(running) = slot
            .as_mut()
            .filter(|running| Arc::ptr_eq(&running.outcome, &outcome))
        else {
            return Err(UiError::from("the sign-in was cancelled"));
        };
        running.task = Some(tauri::async_runtime::spawn(finish_sign_in(
            flow,
            state.client.clone(),
            state.sessions.clone(),
            state.log.clone(),
            id.clone(),
            outcome.clone(),
        )));
    }
    if let Err(error) = app.opener().open_url(authorize_url.as_str(), None::<&str>) {
        end_sign_in(&state, &outcome);
        return Err(error.into());
    }
    state.log.info(&format!(
        "sign-in to {id} started; waiting for the browser on port {port}"
    ));
    Ok(())
}

/// Waits for the browser, redeems the code and stores the session; the outcome is left for the UI to read.
async fn finish_sign_in(
    flow: Flow,
    client: ApiClient,
    sessions: SessionStore,
    log: Arc<Logger>,
    id: String,
    outcome: SignInOutcome,
) {
    let result = match flow.finish(&client, signin::DEADLINE).await {
        Ok(redeemed) => {
            let session = Session::new(
                redeemed.session,
                redeemed.expires_at,
                redeemed.display_name.clone(),
            );
            match sessions.save(&id, &session) {
                Ok(()) => {
                    log.info(&format!("signed in to {id}"));
                    Ok(redeemed.display_name)
                }
                Err(error) => Err(format!("the sign-in could not be stored: {error}")),
            }
        }
        Err(error) => Err(error.to_string()),
    };
    if let Err(message) = &result {
        log.warn(&format!("sign-in to {id} failed: {message}"));
    }
    *lock(&outcome) = Some(result);
}

#[tauri::command]
fn signin_progress(state: State<'_, AppState>) -> SignInProgress {
    let mut slot = lock(&state.signin);
    let Some(running) = slot.as_ref() else {
        return SignInProgress::Idle;
    };
    let id = running.id.clone();
    let Some(result) = lock(&running.outcome).take() else {
        return SignInProgress::Waiting { id };
    };
    *slot = None;
    match result {
        Ok(display_name) => SignInProgress::Done { id, display_name },
        Err(message) => SignInProgress::Failed { id, message },
    }
}

/// Stops waiting for the browser; the listener closes with the task. Before the server has answered, the start finds its slot gone and opens no browser.
#[tauri::command]
fn signin_cancel(state: State<'_, AppState>) {
    if let Some(running) = lock(&state.signin).take() {
        if let Some(task) = running.task {
            task.abort();
        }
        state
            .log
            .info(&format!("sign-in to {} cancelled", running.id));
    }
}

/// Tells the server and removes the stored sign-in; the file goes even when the server cannot be told.
#[tauri::command]
async fn sign_out(id: String, state: State<'_, AppState>) -> Result<(), UiError> {
    let entry = entry_of(&state, &id)?;
    if let Some(session) = load_session(&state, &id)
        && let Err(error) = state
            .client
            .logout(&entry.backend_url, &session.session)
            .await
    {
        state.log.warn(&format!(
            "logout at {id} not confirmed by the server: {error}"
        ));
    }
    state.sessions.delete(&id)?;
    state.log.info(&format!("signed out of {id}"));
    Ok(())
}

fn play_view(state: &AppState) -> PlayView {
    let run = lock(&state.play);
    PlayView {
        state: run.machine.state(),
        id: run.id.clone(),
        notice: run.machine.notice().cloned(),
    }
}

fn apply_play(state: &AppState, event: Event) -> Option<Effect> {
    lock(&state.play).machine.apply(event)
}

/// The files of the server as Play needs them: nothing left to install and the hashed runtime files intact.
async fn verify_files(state: &AppState, id: &str) -> Result<ServerContext, String> {
    let context = server_context(state, id)
        .await
        .map_err(ContextError::reason)?;
    let installer = Installer::new(
        context.steam.clone(),
        context.folders.clone(),
        Cancel::default(),
        Arc::new(Mutex::new(Phase::Planning)),
    );
    let manifest = context.manifest.clone();
    let (checked, notices) = tauri::async_runtime::spawn_blocking(move || {
        let mut notices = Vec::new();
        let checked = installer
            .plan(&manifest, false)
            .map_err(|error| error.to_string())
            .and_then(|plan| {
                let to_place = plan.runtime.iter().filter(|file| !file.present).count();
                if plan.download.is_empty() && plan.remove.is_empty() && to_place == 0 {
                    Ok(())
                } else {
                    let action = if plan.installed_version().is_some() {
                        "Update"
                    } else {
                        "Install"
                    };
                    Err(format!(
                        "the server's files are not in place ({} to download, {} to remove, {} game files to place); press {action} first",
                        plan.download.len(),
                        plan.remove.len(),
                        to_place
                    ))
                }
            })
            .and_then(|()| {
                installer
                    .check_runtime(&manifest, &mut notices)
                    .map_err(|error| error.to_string())
            });
        (checked, notices)
    })
    .await
    .map_err(|error| error.to_string())?;
    for notice in notices {
        state.log.warn(&notice);
    }
    checked.map(|()| context)
}

/// Asks the server for a play token with the stored sign-in; a sign-in it refuses for good is removed.
async fn request_token(state: &AppState, id: &str, origin: &Url) -> Result<PlayGrant, String> {
    let Some(session) = load_session(state, id) else {
        return Err("sign in to this server first".to_string());
    };
    match state.client.play(origin, &session.session).await {
        Ok(grant) => {
            state.log.info(&format!("play token granted by {id}"));
            Ok(grant)
        }
        Err(ApiError::Unauthorized) => {
            forget_session(state, id, "no longer valid");
            Err(ApiError::Unauthorized.to_string())
        }
        Err(error @ ApiError::Refused { .. }) => {
            if let ApiError::Refused { code, .. } = &error
                && code == "banned"
            {
                forget_session(state, id, "banned");
            }
            state.log.warn(&format!("play refused by {id}: {error:?}"));
            Err(error.to_string())
        }
        Err(error) => {
            state
                .log
                .warn(&format!("play request to {id} failed: {error}"));
            Err(format!("the play request failed: {error}"))
        }
    }
}

async fn launch_game(
    state: &AppState,
    context: ServerContext,
    grant: PlayGrant,
) -> Result<Game, String> {
    let paths = launch_paths(&state.dirs);
    let launch = LaunchFile::new(
        grant.server.host,
        grant.server.port,
        grant.play_token,
        grant.expires_at,
    );
    let manifest = context.manifest;
    let game_folder = context.folders.game;
    tauri::async_runtime::spawn_blocking(move || {
        Game::start(&Start {
            paths: &paths,
            game_folder: &game_folder,
            loader: &manifest.launch.loader,
            plugins: &manifest.plugins,
            launch: &launch,
        })
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Looks at the loader every half second; once it is gone the files are back and the run returns to idle.
fn watch_game(play: Arc<Mutex<PlayRun>>, log: Arc<Logger>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(GAME_POLL).await;
            let mut run = lock(&play);
            let Some(game) = run.game.as_mut() else {
                return;
            };
            let (code, problem) = match game.poll() {
                Ok(None) => continue,
                Ok(Some(status)) => {
                    log.info(&format!("the game exited with {status}"));
                    (status.code(), None)
                }
                Err(error) => {
                    log.warn(&format!("the game run could not be put right: {error}"));
                    (None, Some(error.to_string()))
                }
            };
            run.game = None;
            run.id = None;
            run.machine.apply(Event::GameExited { code, problem });
            return;
        }
    });
}

/// Runs one Play from the files check to the loader's start; each step reports to the machine, which decides the next.
async fn run_play(state: &AppState, id: &str) {
    let context = match verify_files(state, id).await {
        Ok(context) => context,
        Err(message) => {
            state.log.warn(&format!("play on {id} stopped: {message}"));
            apply_play(state, Event::FilesRefused(message));
            return;
        }
    };
    if apply_play(state, Event::FilesVerified) != Some(Effect::RequestToken) {
        return;
    }
    let grant = match request_token(state, id, &context.origin).await {
        Ok(grant) => grant,
        Err(message) => {
            apply_play(state, Event::TokenRefused(message));
            return;
        }
    };
    if apply_play(state, Event::TokenGranted) != Some(Effect::Launch) {
        return;
    }
    match launch_game(state, context, grant).await {
        Ok(game) => {
            state.log.info(&format!("the game started for {id}"));
            let mut run = lock(&state.play);
            run.game = Some(game);
            run.machine.apply(Event::Launched);
            drop(run);
            watch_game(state.play.clone(), state.log.clone());
        }
        Err(message) => {
            state
                .log
                .warn(&format!("the game could not start for {id}: {message}"));
            apply_play(state, Event::LaunchFailed(message));
        }
    }
}

/// Play: files checked, a token requested, the game started; the answer is where the run stands afterwards.
#[tauri::command]
async fn play(id: String, state: State<'_, AppState>) -> Result<PlayView, UiError> {
    entry_of(&state, &id)?;
    {
        let mut run = lock(&state.play);
        if run.machine.apply(Event::Play) != Some(Effect::VerifyFiles) {
            return Err(UiError::from("a game is already starting or running"));
        }
        run.id = Some(id.clone());
    }
    run_play(&state, &id).await;
    if lock(&state.play).machine.state() == play::State::Idle {
        lock(&state.play).id = None;
    }
    Ok(play_view(&state))
}

#[tauri::command]
fn play_state(state: State<'_, AppState>) -> PlayView {
    play_view(&state)
}

/// Reconnect: a new token for the running game, written where the client reads it at its next connect attempt.
#[tauri::command]
async fn reconnect(id: String, state: State<'_, AppState>) -> Result<PlayView, UiError> {
    let entry = entry_of(&state, &id)?;
    {
        let mut run = lock(&state.play);
        if run.id.as_deref() != Some(id.as_str())
            || run.machine.apply(Event::Reconnect) != Some(Effect::RequestReconnectToken)
        {
            return Err(UiError::from("no game is running for this server"));
        }
    }
    let event = match request_token(&state, &id, &entry.backend_url).await {
        Ok(grant) => {
            let launch = LaunchFile::new(
                grant.server.host,
                grant.server.port,
                grant.play_token,
                grant.expires_at,
            );
            let run = lock(&state.play);
            match run
                .game
                .as_ref()
                .map(|game| game.rewrite_launch_file(&launch))
            {
                Some(Ok(())) => {
                    state
                        .log
                        .info(&format!("a new play token written for {id}"));
                    Event::Reconnected
                }
                Some(Err(error)) => {
                    Event::ReconnectFailed(format!("the new token could not be written: {error}"))
                }
                None => Event::ReconnectFailed("the game is no longer running".to_string()),
            }
        }
        Err(message) => Event::ReconnectFailed(message),
    };
    apply_play(&state, event);
    Ok(play_view(&state))
}

/// A release build has no console, so a failure to start is shown in a message box before the exit.
fn show_fatal(caption: &str, error: &tauri::Error) {
    let message = match error {
        tauri::Error::Setup(inner) => inner.to_string(),
        other => other.to_string(),
    };
    let text = HSTRING::from(format!("The launcher cannot start.\n\n{message}"));
    let caption = HSTRING::from(caption);
    // SAFETY: both strings outlive the call and no owner window is passed.
    unsafe { MessageBoxW(None, &text, &caption, MB_OK | MB_ICONERROR) };
}

pub fn run() -> tauri::Result<()> {
    let context = tauri::generate_context!();
    let product = context.config().product_name.clone().unwrap_or_default();
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let state = AppState::build(app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            launcher_info,
            update_check,
            update_install,
            update_progress,
            load_servers,
            list_servers,
            server_status,
            server_page,
            install_state,
            install_start,
            install_progress,
            install_cancel,
            preview_server,
            add_server,
            remove_server,
            open_link,
            get_settings,
            set_base_folder,
            signin_state,
            signin_start,
            signin_progress,
            signin_cancel,
            sign_out,
            play,
            play_state,
            reconnect
        ])
        .run(context);
    if let Err(error) = &result {
        show_fatal(&product, error);
    }
    result
}

// SPDX-License-Identifier: AGPL-3.0-or-later
//! The self-update: one check at start, then the download, the signature check and the installer of the new build.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Error as PluginError, Update, UpdaterExt};

use crate::config;

/// How long connecting and the whole check may take; the download has its own bounds below.
const TIMEOUT: Duration = Duration::from_secs(10);
/// A download that sends nothing for this long is given up, however large the installer is.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A download that is not finished by then is given up, however slowly the host trickles.
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(600);
/// An installer past this size is refused while it still downloads; the launcher's own installers stay far below it.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// How often the running download is measured against the size cap.
const CAP_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("this build has no signing key in tauri.conf.json, so updates are not checked")]
    Unconfigured,
    #[error(transparent)]
    Key(#[from] config::ConfigError),
    #[error("no update is pending")]
    NothingPending,
    #[error("the update is already being installed")]
    Busy,
    #[error("the update is larger than the launcher accepts")]
    TooLarge,
    #[error("the download did not finish within {} minutes", DOWNLOAD_DEADLINE.as_secs() / 60)]
    Timeout,
    #[error("the download stopped before the installer was complete")]
    Interrupted,
    #[error("{}", plain(.0))]
    Plugin(#[from] PluginError),
}

/// What the check at start found; `Available` holds the launcher until the update is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Check {
    UpToDate,
    Available {
        version: String,
    },
    /// The check could not be made, so this build goes on; the reason is fit for the log and the player.
    Skipped {
        reason: String,
    },
}

/// Where the install of the pending update stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Progress {
    #[default]
    Idle,
    Downloading {
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    /// The installer is being started; the launcher exits as soon as it runs.
    Installing,
    Failed {
        message: String,
    },
}

/// The one update that may be pending, and how far its install is.
#[derive(Default)]
pub struct Slot {
    pending: Mutex<Option<Update>>,
    progress: Mutex<Progress>,
}

impl Slot {
    /// Asks the endpoints for a newer build; a check that cannot be made is a skip, never a stop.
    pub async fn check<R: Runtime>(&self, app: &AppHandle<R>, user_agent: &str) -> Check {
        *lock(&self.pending) = None;
        *lock(&self.progress) = Progress::Idle;
        match find_update(app, user_agent).await {
            Ok(None) => Check::UpToDate,
            Ok(Some(update)) => {
                let version = update.version.clone();
                *lock(&self.pending) = Some(update);
                Check::Available { version }
            }
            Err(error) => Check::Skipped {
                reason: error.to_string(),
            },
        }
    }

    /// Downloads the pending update; the bytes come back only once their signature has checked out.
    pub async fn download(&self) -> Result<Vec<u8>, UpdateError> {
        let update = lock(&self.pending)
            .clone()
            .ok_or(UpdateError::NothingPending)?;
        {
            let mut progress = lock(&self.progress);
            if matches!(
                *progress,
                Progress::Downloading { .. } | Progress::Installing
            ) {
                return Err(UpdateError::Busy);
            }
            *progress = Progress::Downloading {
                bytes_done: 0,
                bytes_total: None,
            };
        }
        let received = |chunk: usize, total: Option<u64>| {
            if let Progress::Downloading {
                bytes_done,
                bytes_total,
            } = &mut *lock(&self.progress)
            {
                *bytes_done += chunk as u64;
                *bytes_total = total;
            }
        };
        let download = update.download(received, || {});
        tokio::pin!(download);
        // The plugin cannot stop a download from inside, so the cap is checked beside it; dropping the request ends it.
        let outcome = tokio::time::timeout(DOWNLOAD_DEADLINE, async {
            loop {
                tokio::select! {
                    result = &mut download => break result.map_err(download_error),
                    () = tokio::time::sleep(CAP_POLL) => {
                        if over_cap(&self.progress()) {
                            break Err(UpdateError::TooLarge);
                        }
                    }
                }
            }
        })
        .await
        .unwrap_or(Err(UpdateError::Timeout));
        outcome.map_err(|error| self.fail(error))
    }

    /// Starts the installer with the verified bytes; when it runs, the launcher exits and the installer starts the new build.
    pub fn install(&self, bytes: &[u8]) -> Result<(), UpdateError> {
        let update = lock(&self.pending)
            .take()
            .ok_or(UpdateError::NothingPending)?;
        *lock(&self.progress) = Progress::Installing;
        update
            .install(bytes)
            .map_err(|error| self.fail(UpdateError::Plugin(error)))
    }

    pub fn progress(&self) -> Progress {
        lock(&self.progress).clone()
    }

    /// A failed step drops the pending update, so the launcher goes on with this build.
    fn fail(&self, error: UpdateError) -> UpdateError {
        *lock(&self.progress) = Progress::Failed {
            message: error.to_string(),
        };
        *lock(&self.pending) = None;
        error
    }
}

/// The newer build the endpoints announce, if any; the key and the endpoints come from tauri.conf.json.
async fn find_update<R: Runtime>(
    app: &AppHandle<R>,
    user_agent: &str,
) -> Result<Option<Update>, UpdateError> {
    if config::signing_key(&app.config().plugins.0)?.is_none() {
        return Err(UpdateError::Unconfigured);
    }
    let agent = user_agent.to_string();
    let updater = app
        .updater_builder()
        .timeout(TIMEOUT)
        .no_proxy()
        .configure_client(move |client| {
            client
                .user_agent(agent.clone())
                .connect_timeout(TIMEOUT)
                .read_timeout(READ_TIMEOUT)
        })
        .build()?;
    Ok(updater.check().await.map_err(without_url)?)
}

/// Whether the download has gone past, or announces more than, the size the launcher accepts.
fn over_cap(progress: &Progress) -> bool {
    match progress {
        Progress::Downloading {
            bytes_done,
            bytes_total,
        } => *bytes_done > MAX_BYTES || bytes_total.is_some_and(|total| total > MAX_BYTES),
        _ => false,
    }
}

/// What a failed download means; a body that broke off is a decode error to reqwest, here a stopped transfer.
fn download_error(error: PluginError) -> UpdateError {
    match error {
        PluginError::Reqwest(inner) if inner.is_decode() && !inner.is_timeout() => {
            UpdateError::Interrupted
        }
        other => UpdateError::Plugin(without_url(other)),
    }
}

/// A reqwest error names the URL it was for; neither the player nor the log gets it.
fn without_url(error: PluginError) -> PluginError {
    match error {
        PluginError::Reqwest(inner) => PluginError::Reqwest(inner.without_url()),
        other => other,
    }
}

/// What a failure of the updater plugin means to the player, in plain words.
fn plain(error: &PluginError) -> String {
    match error {
        PluginError::Minisign(_) | PluginError::Base64(_) | PluginError::SignatureUtf8(_) => {
            "the signature of the update is not valid".to_string()
        }
        PluginError::SignedVersionMismatch { .. } | PluginError::MissingSignedVersion => {
            "the update was signed for another version than the one announced".to_string()
        }
        PluginError::ReleaseNotFound => "the update host did not answer with a release".to_string(),
        PluginError::TargetNotFound(_) | PluginError::TargetsNotFound(_) => {
            "the release has no build for this platform".to_string()
        }
        PluginError::Reqwest(error) if error.is_timeout() => {
            "the update host did not answer in time".to_string()
        }
        PluginError::Reqwest(error) if error.is_decode() => {
            "the update host answered with something that is not a release".to_string()
        }
        PluginError::Reqwest(error) => format!("the update host could not be reached: {error}"),
        PluginError::Serialization(error) => format!("the release is not valid: {error}"),
        PluginError::Network(text) => format!("the download failed: {text}"),
        PluginError::Io(error) => format!("the installer could not be started: {error}"),
        other => other.to_string(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

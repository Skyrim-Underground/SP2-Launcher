// SPDX-License-Identifier: AGPL-3.0-or-later
//! The self-update against a latest.json and an installer served by the Node static file server in test/.
//! The app is a mock Tauri app with the updater plugin, so the plugin's own check and download run without a window.

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::NodeServer;
use minisign::KeyPair;
use sug_launcher_lib::update::{Check, Progress, Slot, UpdateError};
use tauri::App;
use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
use tempfile::TempDir;

const USER_AGENT: &str = "sug-launcher-tests/0";
const INSTALLER: &str = "launcher-setup.exe";
const LATEST: &str = "latest.json";
const CURRENT: &str = "0.1.0";
const TAURI_CONF: &str = include_str!("../tauri.conf.json");

/// A minisign key made in memory, with the public key encoded as tauri.conf.json carries it.
struct TestKey {
    pair: KeyPair,
    pubkey: String,
}

impl TestKey {
    fn new() -> Self {
        let pair = KeyPair::generate_unencrypted_keypair().unwrap();
        let pubkey = STANDARD.encode(pair.pk.to_box().unwrap().into_string());
        Self { pair, pubkey }
    }

    /// The .sig content the Tauri CLI writes: the signature file in base64, its trusted comment naming the version.
    fn sign(&self, bytes: &[u8], version: &str) -> String {
        self.sign_with_comment(
            bytes,
            &format!("timestamp:1760000000\tfile:{INSTALLER}\tversion:{version}"),
        )
    }

    fn sign_with_comment(&self, bytes: &[u8], trusted: &str) -> String {
        let signature = minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            bytes,
            Some(trusted),
            None,
        )
        .unwrap()
        .into_string();
        STANDARD.encode(signature)
    }
}

/// A folder served by the static server, holding the installer and the latest.json that points at it.
struct Served {
    dir: TempDir,
    server: NodeServer,
}

impl Served {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let server = NodeServer::start("static-files.mjs", &["--root", root.as_str()]);
        Self { dir, server }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.dir.path().join(name), bytes).unwrap();
    }

    fn url(&self, name: &str) -> String {
        self.route("files", name)
    }

    /// The address of `name` under one of the static server's routes.
    fn route(&self, route: &str, name: &str) -> String {
        self.server
            .origin
            .join(&format!("/{route}/{name}"))
            .unwrap()
            .to_string()
    }

    /// Serves `installer` under its name and announces `version` with `signature` for it.
    fn publish(&self, version: &str, installer: &[u8], signature: &str) {
        self.publish_at(version, installer, signature, &self.url(INSTALLER));
    }

    /// Like `publish`, with the installer announced at `url` instead of its plain address.
    fn publish_at(&self, version: &str, installer: &[u8], signature: &str, url: &str) {
        self.write(INSTALLER, installer);
        self.write(LATEST, latest_json(version, url, signature).as_bytes());
    }
}

/// The static layout sug-website serves, with the one platform this launcher is built for.
fn latest_json(version: &str, url: &str, signature: &str) -> String {
    format!(
        r#"{{"version":"{version}","notes":"test release","pub_date":"2026-10-14T00:00:00Z","platforms":{{"windows-x86_64":{{"signature":"{signature}","url":"{url}"}}}}}}"#
    )
}

/// A mock app with the updater plugin set up as tauri.conf.json ships it, except for the key, the endpoint and the running version.
fn mock_app(pubkey: &str, endpoint: &str, current: &str) -> App<MockRuntime> {
    let conf: serde_json::Value = serde_json::from_str(TAURI_CONF).unwrap();
    let mut updater = conf["plugins"]["updater"].clone();
    updater["pubkey"] = serde_json::Value::String(pubkey.to_string());
    updater["endpoints"] = serde_json::json!([endpoint]);
    let mut context = mock_context(noop_assets());
    context
        .config_mut()
        .plugins
        .0
        .insert("updater".to_string(), updater);
    context.package_info_mut().version = current.parse().unwrap();
    mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(context)
        .unwrap()
}

fn installer_bytes() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 253) as u8).collect()
}

#[tokio::test]
async fn a_newer_signed_build_is_found_and_its_download_verified() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    served.publish("9.9.9", &installer, &key.sign(&installer, "9.9.9"));
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert_eq!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available {
            version: "9.9.9".to_string()
        }
    );
    assert_eq!(slot.progress(), Progress::Idle);
    let bytes = slot.download().await.unwrap();
    assert_eq!(bytes, installer);
    let size = installer.len() as u64;
    assert_eq!(
        slot.progress(),
        Progress::Downloading {
            bytes_done: size,
            bytes_total: Some(size)
        }
    );
}

#[tokio::test]
async fn a_lower_or_equal_version_is_not_an_update() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    for version in ["0.0.9", CURRENT] {
        served.publish(version, &installer, &key.sign(&installer, version));
        let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
        let slot = Slot::default();
        assert_eq!(
            slot.check(app.handle(), USER_AGENT).await,
            Check::UpToDate,
            "version {version}"
        );
        assert!(matches!(
            slot.download().await,
            Err(UpdateError::NothingPending)
        ));
    }
}

#[tokio::test]
async fn a_tampered_installer_is_refused() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    served.publish("9.9.9", &installer, &key.sign(&installer, "9.9.9"));
    let mut tampered = installer.clone();
    tampered[1000] ^= 0x01;
    served.write(INSTALLER, &tampered);
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert!(matches!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available { .. }
    ));
    let refused = slot.download().await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        "the signature of the update is not valid"
    );
    assert_eq!(
        slot.progress(),
        Progress::Failed {
            message: "the signature of the update is not valid".to_string()
        }
    );
    assert!(matches!(
        slot.install(&tampered),
        Err(UpdateError::NothingPending)
    ));
}

#[tokio::test]
async fn a_signature_from_another_key_is_refused() {
    let key = TestKey::new();
    let other = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    served.publish("9.9.9", &installer, &other.sign(&installer, "9.9.9"));
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert!(matches!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available { .. }
    ));
    assert_eq!(
        slot.download().await.unwrap_err().to_string(),
        "the signature of the update is not valid"
    );
}

#[tokio::test]
async fn a_release_announced_as_another_version_than_it_was_signed_for_is_refused() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    let unnamed = key.sign_with_comment(&installer, "timestamp:1760000000\tfile:old-setup.exe");
    for signature in [key.sign(&installer, "0.2.0"), unnamed] {
        served.publish("9.9.9", &installer, &signature);
        let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
        let slot = Slot::default();
        assert!(matches!(
            slot.check(app.handle(), USER_AGENT).await,
            Check::Available { .. }
        ));
        assert_eq!(
            slot.download().await.unwrap_err().to_string(),
            "the update was signed for another version than the one announced"
        );
    }
}

#[tokio::test]
async fn a_check_that_cannot_be_made_is_skipped() {
    let key = TestKey::new();
    let served = Served::new();
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    let Check::Skipped { reason } = slot.check(app.handle(), USER_AGENT).await else {
        panic!("a missing latest.json must skip the update");
    };
    assert_eq!(reason, "the update host did not answer with a release");
    served.write(LATEST, b"not json");
    let Check::Skipped { reason } = slot.check(app.handle(), USER_AGENT).await else {
        panic!("a damaged latest.json must skip the update");
    };
    assert_eq!(
        reason,
        "the update host answered with something that is not a release"
    );
    served.write(LATEST, br#"{"platforms":{}}"#);
    let Check::Skipped { reason } = slot.check(app.handle(), USER_AGENT).await else {
        panic!("a latest.json without a version must skip the update");
    };
    assert!(reason.starts_with("the release is not valid: "), "{reason}");
    let unreachable = mock_app(&key.pubkey, "http://127.0.0.1:1/latest.json", CURRENT);
    let Check::Skipped { reason } = slot.check(unreachable.handle(), USER_AGENT).await else {
        panic!("an unreachable host must skip the update");
    };
    assert!(
        reason.starts_with("the update host could not be reached"),
        "{reason}"
    );
    assert!(!reason.contains("127.0.0.1:1"), "{reason}");
    assert!(matches!(
        slot.download().await,
        Err(UpdateError::NothingPending)
    ));
}

#[tokio::test]
async fn an_installer_larger_than_the_launcher_accepts_is_refused_while_it_downloads() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    // The overrun route sends the installer and then padding without end, so only the cap stops the download.
    served.publish_at(
        "9.9.9",
        &installer,
        &key.sign(&installer, "9.9.9"),
        &served.route("overrun", INSTALLER),
    );
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert!(matches!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available { .. }
    ));
    let refused = slot.download().await.unwrap_err();
    assert!(matches!(refused, UpdateError::TooLarge), "{refused}");
    assert_eq!(
        slot.progress(),
        Progress::Failed {
            message: "the update is larger than the launcher accepts".to_string()
        }
    );
    assert!(matches!(
        slot.install(&installer),
        Err(UpdateError::NothingPending)
    ));
}

#[tokio::test]
async fn a_transfer_cut_short_is_refused() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    let cut = format!("cut/{}", installer.len() / 2);
    served.publish_at(
        "9.9.9",
        &installer,
        &key.sign(&installer, "9.9.9"),
        &served.route(&cut, INSTALLER),
    );
    let app = mock_app(&key.pubkey, &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert!(matches!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available { .. }
    ));
    let refused = slot.download().await.unwrap_err();
    assert!(matches!(refused, UpdateError::Interrupted), "{refused}");
    assert_eq!(
        slot.progress(),
        Progress::Failed {
            message: "the download stopped before the installer was complete".to_string()
        }
    );
    assert!(matches!(
        slot.download().await,
        Err(UpdateError::NothingPending)
    ));
}

#[tokio::test]
async fn without_a_signing_key_no_check_is_made() {
    let key = TestKey::new();
    let installer = installer_bytes();
    let served = Served::new();
    served.publish("9.9.9", &installer, &key.sign(&installer, "9.9.9"));
    let app = mock_app("", &served.url(LATEST), CURRENT);
    let slot = Slot::default();
    assert_eq!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Skipped {
            reason: "this build has no signing key in tauri.conf.json, so updates are not checked"
                .to_string()
        }
    );
    assert!(matches!(
        slot.download().await,
        Err(UpdateError::NothingPending)
    ));
}

/// The version a signature from the Tauri CLI names in its trusted comment.
fn signed_version(signature_base64: &str) -> String {
    let text = String::from_utf8(STANDARD.decode(signature_base64.trim()).unwrap()).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix("trusted comment: "))
        .and_then(|comment| {
            comment
                .split('\t')
                .find_map(|field| field.strip_prefix("version:"))
        })
        .expect("the trusted comment names the version the installer was signed for")
        .to_string()
}

/// Needs an installer signed by `npx tauri build` with its .sig next to it, and the matching public key.
/// SUG_LAUNCHER_UPDATE_INSTALLER names the -setup.exe; SUG_LAUNCHER_UPDATE_PUBKEY is the pubkey as tauri.conf.json carries it.
#[tokio::test]
#[ignore]
async fn the_installer_signed_by_the_build_updates_from_the_static_server() {
    let installer_path = PathBuf::from(
        env::var_os("SUG_LAUNCHER_UPDATE_INSTALLER").expect("SUG_LAUNCHER_UPDATE_INSTALLER"),
    );
    let pubkey = env::var("SUG_LAUNCHER_UPDATE_PUBKEY").expect("SUG_LAUNCHER_UPDATE_PUBKEY");
    let installer = fs::read(&installer_path).unwrap();
    let signature = fs::read_to_string(sig_path(&installer_path)).unwrap();
    let version = signed_version(&signature);
    let served = Served::new();
    served.publish(&version, &installer, signature.trim());
    // The running version is set below every release, so the build under test counts as the update.
    let app = mock_app(&pubkey, &served.url(LATEST), "0.0.1");
    let slot = Slot::default();
    assert_eq!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available {
            version: version.clone()
        }
    );
    let bytes = slot.download().await.unwrap();
    assert_eq!(bytes.len(), installer.len());
    assert_eq!(bytes, installer);
    let mut tampered = installer.clone();
    tampered[installer.len() / 2] ^= 0x01;
    served.write(INSTALLER, &tampered);
    assert!(matches!(
        slot.check(app.handle(), USER_AGENT).await,
        Check::Available { .. }
    ));
    assert_eq!(
        slot.download().await.unwrap_err().to_string(),
        "the signature of the update is not valid"
    );
}

fn sig_path(installer: &Path) -> PathBuf {
    let mut name = installer.file_name().unwrap().to_os_string();
    name.push(".sig");
    installer.with_file_name(name)
}

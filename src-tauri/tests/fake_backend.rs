// SPDX-License-Identifier: AGPL-3.0-or-later
//! The api and servers modules against the Node fake backend in test/.

mod common;

use std::collections::HashSet;

use common::NodeServer;
use sug_launcher_lib::api::{ApiClient, ServerCheck};
use sug_launcher_lib::manifest::{self, Context};
use sug_launcher_lib::servers::ServerStore;

fn client() -> ApiClient {
    ApiClient::new("sug-launcher-tests/0").unwrap()
}

fn backend(args: &[&str]) -> NodeServer {
    NodeServer::start("fake-backend.mjs", args)
}

#[tokio::test]
async fn api_version_two_needs_a_newer_launcher() {
    let backend = backend(&["--api-version", "2"]);
    assert_eq!(
        client().server(&backend.origin).await.unwrap(),
        ServerCheck::LauncherTooOld { api_version: 2 }
    );
}

#[tokio::test]
async fn api_version_one_gives_a_usable_entry_with_status() {
    let backend = backend(&["--api-version", "1"]);
    let ServerCheck::Ok { info } = client().server(&backend.origin).await.unwrap() else {
        panic!("expected a usable server");
    };
    let root = tempfile::tempdir().unwrap();
    let mut store =
        ServerStore::load(&root.path().join("config"), &root.path().join("cache")).unwrap();
    let entry = store
        .add_manual(backend.origin.clone(), info.name, 1_760_000_000)
        .unwrap();
    assert_eq!(entry.name, "Fake Server");
    assert!(!entry.listed);
    assert_eq!(store.entries().len(), 1);
    let status = client().status(&backend.origin).await.unwrap();
    assert!(status.online);
    assert_eq!(status.players, 3);
    assert_eq!(status.max_players, 64);
    assert_eq!(status.sign_in, "ok");
}

#[tokio::test]
async fn the_served_manifest_parses_against_the_announced_runtime() {
    let fixture = format!(
        "{}/../test/fixtures/manifest.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let backend = backend(&["--manifest", &fixture]);
    let ServerCheck::Ok { info } = client().server(&backend.origin).await.unwrap() else {
        panic!("expected a usable server");
    };
    let bytes = client()
        .manifest(&backend.origin, &info.manifest_url)
        .await
        .unwrap();
    let steam_data: HashSet<String> = ["skyrim.esm", "update.esm", "dawnguard.esm"]
        .into_iter()
        .map(String::from)
        .collect();
    let manifest = manifest::parse(
        &bytes,
        &Context {
            game_runtime: &info.game_runtime,
            steam_data: &steam_data,
        },
    )
    .unwrap();
    assert_eq!(manifest.version, "2026.10.13-1");
    assert_eq!(manifest.native_files().count(), 2);
}

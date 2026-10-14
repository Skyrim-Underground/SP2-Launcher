// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sign-in and Play against the Node fake backend in test/, with the test playing the browser.

mod common;

use std::time::Duration;

use common::NodeServer;
use reqwest::redirect;
use serde::Deserialize;
use sug_launcher_lib::api::{Access, ApiClient, ApiError};
use sug_launcher_lib::secrets::{Session, SessionStore};
use sug_launcher_lib::servers::server_id;
use sug_launcher_lib::signin::{AUTHORIZE_URL, DEADLINE, Flow, SignInError};
use url::Url;

const CALLBACK_ROUTE: &str = "/api/v1/users/login-discord/callback";

fn client() -> ApiClient {
    ApiClient::new("sug-launcher-tests/0").unwrap()
}

fn backend(args: &[&str]) -> NodeServer {
    NodeServer::start("fake-backend.mjs", args)
}

/// A browser that stops at redirects, so the test can see where the backend sends it, and gives up rather than hang.
fn browser() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap()
}

/// One request the fake backend served, with the Authorization header it carried.
#[derive(Debug, Deserialize)]
struct Served {
    method: String,
    path: String,
    authorization: Option<String>,
}

async fn served_by(origin: &Url) -> Vec<Served> {
    let bytes = client()
        .get_bytes(&origin.join("/fake/log").unwrap(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Plays Discord's part: lands on the backend's callback with a code and returns where the backend sends the browser.
async fn backend_callback(origin: &Url, state: &str, discord_code: &str) -> Url {
    let callback = origin
        .join(&format!(
            "{CALLBACK_ROUTE}?code={discord_code}&state={state}"
        ))
        .unwrap();
    let redirect = browser().get(callback).send().await.unwrap();
    assert_eq!(redirect.status(), 302);
    assert_eq!(redirect.headers().get("cache-control").unwrap(), "no-store");
    Url::parse(
        redirect
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}

/// Plays Discord and the browser: the callback, then its redirect followed to the launcher's waiting listener.
async fn come_back(origin: &Url, state: &str, discord_code: &str) -> (Url, reqwest::Response) {
    let location = backend_callback(origin, state, discord_code).await;
    let answer = browser().get(location.clone()).send().await.unwrap();
    (location, answer)
}

fn query_value(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
        .unwrap()
}

fn state_of(authorize_url: &Url) -> String {
    query_value(authorize_url, "state")
}

/// A sign-in from the start request to the stored session.
async fn sign_in(origin: &Url, store: &SessionStore) -> Session {
    let flow = Flow::start(&client(), origin).await.unwrap();
    let state = state_of(flow.authorize_url());
    let port = flow.port();
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    let (location, answer) = come_back(origin, &state, "discord-code").await;
    assert_eq!(location.host_str(), Some("127.0.0.1"));
    assert_eq!(location.port(), Some(port));
    assert_eq!(answer.status(), 200);
    let redeemed = finishing.await.unwrap().unwrap();
    let session = Session::new(redeemed.session, redeemed.expires_at, redeemed.display_name);
    store.save(&server_id(origin), &session).unwrap();
    session
}

#[tokio::test]
async fn the_full_sign_in_flow_gives_a_session_that_me_and_logout_accept() {
    let backend = backend(&["--display-name", "Tester"]);
    let origin = &backend.origin;
    let flow = Flow::start(&client(), origin).await.unwrap();
    assert!(flow.authorize_url().as_str().starts_with(AUTHORIZE_URL));
    assert!(flow.port() >= 1024);
    let state = state_of(flow.authorize_url());
    assert_eq!(state.len(), 43);
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    let (location, answer) = come_back(origin, &state, "discord-code").await;
    assert_eq!(location.path(), "/callback");
    assert_eq!(answer.status(), 200);
    assert!(answer.text().await.unwrap().contains("Signed in"));
    let redeemed = finishing.await.unwrap().unwrap();
    println!(
        "session prefix {}, {} chars, display name {}",
        &redeemed.session[..5],
        redeemed.session.len(),
        redeemed.display_name
    );
    assert!(redeemed.session.starts_with("sugs_"));
    assert_eq!(redeemed.session.len(), 48);
    assert_eq!(redeemed.display_name, "Tester");
    assert!(redeemed.expires_at > 1_700_000_000_000);
    let me = client().me(origin, &redeemed.session).await.unwrap();
    assert_eq!(me.access, Access::Ok);
    assert_eq!(me.display_name, "Tester");
    client().logout(origin, &redeemed.session).await.unwrap();
    assert!(matches!(
        client().me(origin, &redeemed.session).await,
        Err(ApiError::Unauthorized)
    ));
    // The code was spent by the redeem, so the same callback cannot be replayed into a second session.
    let served = served_by(origin).await;
    let paths: Vec<&str> = served.iter().map(|entry| entry.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/v1/users/login-discord/start",
            CALLBACK_ROUTE,
            "/api/v1/users/login-discord/redeem",
            "/api/v1/users/me",
            "/api/v1/users/logout",
            "/api/v1/users/me",
            "/fake/log"
        ]
    );
}

#[tokio::test]
async fn a_foreign_state_is_ignored_and_the_own_state_still_completes() {
    let backend = backend(&[]);
    let origin = &backend.origin;
    let flow = Flow::start(&client(), origin).await.unwrap();
    let state = state_of(flow.authorize_url());
    let port = flow.port();
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    // A second flow at the same backend: its state and a code the backend issued for it land on the first flow's listener.
    let other = Flow::start(&client(), origin).await.unwrap();
    let other_state = state_of(other.authorize_url());
    assert_ne!(other_state, state);
    let other_location = backend_callback(origin, &other_state, "discord-code").await;
    assert_eq!(other_location.port(), Some(other.port()));
    assert_ne!(other_location.port(), Some(port));
    let other_code = query_value(&other_location, "code");
    drop(other);
    let forged = Url::parse(&format!(
        "http://127.0.0.1:{port}/callback?state={other_state}&code={other_code}"
    ))
    .unwrap();
    let forged_answer = browser().get(forged).send().await.unwrap();
    assert_eq!(forged_answer.status().as_u16(), 404);
    let no_state = Url::parse(&format!(
        "http://127.0.0.1:{port}/callback?code={other_code}"
    ))
    .unwrap();
    assert_eq!(
        browser()
            .get(no_state)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !finishing.is_finished(),
        "a foreign state must not end the wait"
    );
    let (_, answer) = come_back(origin, &state, "discord-code").await;
    assert_eq!(answer.status(), 200);
    let redeemed = finishing.await.unwrap().unwrap();
    assert!(redeemed.session.starts_with("sugs_"));
    // The redeem carried the first flow's own state and code, never the forged one.
    let served = served_by(origin).await;
    assert_eq!(
        served
            .iter()
            .filter(|entry| entry.path.ends_with("/redeem"))
            .count(),
        1
    );
}

#[tokio::test]
async fn the_listener_closes_after_the_deadline_and_the_port_is_free() {
    let backend = backend(&[]);
    let flow = Flow::start(&client(), &backend.origin).await.unwrap();
    let port = flow.port();
    let ended = flow.finish(&client(), Duration::from_millis(500)).await;
    assert!(matches!(ended, Err(SignInError::Timeout(1))), "{ended:?}");
    let freed = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(freed.is_ok(), "port {port} must be free: {freed:?}");
    println!("listener on port {port} closed after the deadline; the port binds again");
}

#[tokio::test]
async fn two_backends_give_two_sessions_each_sent_only_to_its_own_origin() {
    let first = backend(&["--name", "First", "--display-name", "One"]);
    let second = backend(&["--name", "Second", "--display-name", "Two"]);
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path());
    let session_a = sign_in(&first.origin, &store).await;
    let session_b = sign_in(&second.origin, &store).await;
    assert_ne!(session_a.session, session_b.session);
    assert_eq!(session_a.display_name, "One");
    assert_eq!(session_b.display_name, "Two");
    let id_a = server_id(&first.origin);
    let id_b = server_id(&second.origin);
    assert_ne!(id_a, id_b);
    assert_eq!(store.load(&id_a, 0).unwrap(), Some(session_a.clone()));
    assert_eq!(store.load(&id_b, 0).unwrap(), Some(session_b.clone()));
    // Each session is used against its own backend, as the launcher does with the file it stored under the server's id.
    client()
        .me(&first.origin, &session_a.session)
        .await
        .unwrap();
    client()
        .me(&second.origin, &session_b.session)
        .await
        .unwrap();
    client()
        .play(&first.origin, &session_a.session)
        .await
        .unwrap();
    client()
        .play(&second.origin, &session_b.session)
        .await
        .unwrap();
    let bearer_a = format!("Bearer {}", session_a.session);
    let bearer_b = format!("Bearer {}", session_b.session);
    for (origin, own, other) in [
        (&first.origin, &bearer_a, &bearer_b),
        (&second.origin, &bearer_b, &bearer_a),
    ] {
        let served = served_by(origin).await;
        let headers: Vec<&String> = served
            .iter()
            .filter_map(|entry| entry.authorization.as_ref())
            .collect();
        println!(
            "{} saw {} requests, {} with Authorization, all its own: {}",
            origin,
            served.len(),
            headers.len(),
            headers.iter().all(|header| *header == own)
        );
        assert_eq!(
            headers.len(),
            2,
            "me and play carry the session, nothing else does"
        );
        assert!(headers.iter().all(|header| *header == own));
        assert!(!headers.contains(&other));
        let with_session: Vec<(&str, &str)> = served
            .iter()
            .filter(|entry| entry.authorization.is_some())
            .map(|entry| (entry.method.as_str(), entry.path.as_str()))
            .collect();
        assert_eq!(
            with_session,
            [("GET", "/api/v1/users/me"), ("POST", "/api/v1/play")]
        );
    }
    // One backend's session means nothing to the other.
    assert!(matches!(
        client().me(&first.origin, &session_b.session).await,
        Err(ApiError::Unauthorized)
    ));
}

#[tokio::test]
async fn play_grants_a_token_or_names_its_refusal_plainly() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path());
    let open = backend(&["--game-host", "play.example.org", "--game-port", "7778"]);
    let session = sign_in(&open.origin, &store).await;
    let grant = client().play(&open.origin, &session.session).await.unwrap();
    assert!(grant.play_token.starts_with("sugp_"));
    assert_eq!(grant.play_token.len(), 48);
    assert_eq!(grant.server.host, "play.example.org");
    assert_eq!(grant.server.port, 7778);
    assert!(grant.expires_at > 1_700_000_000_000);
    assert!(matches!(
        client()
            .play(&open.origin, "sugs_NOTaSESSIONnotASESSIONnotASESSIONnotASE")
            .await,
        Err(ApiError::Unauthorized)
    ));
    for (args, status, code, text) in [
        (
            vec!["--access", "banned"],
            403,
            "banned",
            "you are banned from this server",
        ),
        (
            vec!["--access", "not_member"],
            403,
            "not_member",
            "you are not a member of this server's Discord",
        ),
        (
            vec!["--access", "missing_role"],
            403,
            "missing_role",
            "your Discord account has no role that allows playing on this server",
        ),
        (
            vec!["--access", "discord_unavailable"],
            503,
            "discord_unavailable",
            "the server cannot reach Discord right now; try again later",
        ),
        (
            vec!["--offline"],
            503,
            "server_offline",
            "the game server is offline",
        ),
        (
            vec!["--full"],
            503,
            "server_full",
            "the game server is full",
        ),
    ] {
        let refusing = backend(&args);
        let session = sign_in(&refusing.origin, &store).await;
        let error = client()
            .play(&refusing.origin, &session.session)
            .await
            .unwrap_err();
        println!("{args:?} -> {error:?} shown as {error}");
        assert!(
            matches!(&error, ApiError::Refused { status: got, code: got_code } if *got == status && got_code == code),
            "{args:?}: {error:?}"
        );
        assert_eq!(error.to_string(), text);
    }
}

#[tokio::test]
async fn me_reports_every_access_state_and_a_ban_with_its_reason() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path());
    for (access, expected) in [
        ("not_member", Access::NotMember),
        ("missing_role", Access::MissingRole),
        ("discord_unavailable", Access::DiscordUnavailable),
    ] {
        let backend = backend(&["--access", access]);
        let session = sign_in(&backend.origin, &store).await;
        let me = client()
            .me(&backend.origin, &session.session)
            .await
            .unwrap();
        assert_eq!(me.access, expected);
        assert_eq!(me.ban_reason, None);
    }
    let banned = backend(&["--access", "banned", "--ban-reason", "test ban"]);
    let session = sign_in(&banned.origin, &store).await;
    let me = client().me(&banned.origin, &session.session).await.unwrap();
    assert_eq!(me.access, Access::Banned);
    assert_eq!(me.ban_reason.as_deref(), Some("test ban"));
    assert!(me.ban_until.is_some());
}

#[tokio::test]
async fn a_refusal_in_the_callback_ends_the_sign_in_with_its_reason() {
    let backend = backend(&["--callback-error", "not_member"]);
    let origin = &backend.origin;
    let flow = Flow::start(&client(), origin).await.unwrap();
    let state = state_of(flow.authorize_url());
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    let (location, answer) = come_back(origin, &state, "discord-code").await;
    assert!(location.query().unwrap().contains("error=not_member"));
    assert_eq!(answer.status(), 200);
    let ended = finishing.await.unwrap();
    assert!(
        matches!(&ended, Err(SignInError::Refused(code)) if code == "not_member"),
        "{ended:?}"
    );
    assert_eq!(
        ended.unwrap_err().to_string(),
        "the sign-in was refused: you are not a member of this server's Discord"
    );
    // Discord sending no code is a denial, however it is phrased.
    let flow = Flow::start(&client(), origin).await.unwrap();
    let state = state_of(flow.authorize_url());
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    let denied = origin
        .join(&format!(
            "{CALLBACK_ROUTE}?error=access_denied&state={state}"
        ))
        .unwrap();
    let redirect = browser().get(denied).send().await.unwrap();
    let location = redirect
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.contains("error=denied"));
    browser().get(&location).send().await.unwrap();
    assert!(matches!(
        finishing.await.unwrap(),
        Err(SignInError::Refused(code)) if code == "denied"
    ));
}

#[tokio::test]
async fn a_sign_in_address_off_discord_is_refused_before_any_browser_opens() {
    let backend = backend(&[
        "--authorize-url",
        "https://discord.com.example.org/oauth2/authorize",
    ]);
    let started = Flow::start(&client(), &backend.origin).await;
    assert!(
        matches!(started, Err(SignInError::AuthorizeUrl)),
        "{:?}",
        started.err()
    );
    let served = served_by(&backend.origin).await;
    assert_eq!(served[0].path, "/api/v1/users/login-discord/start");
    assert_eq!(
        served.len(),
        2,
        "the start request and this log read, nothing else"
    );
}

#[tokio::test]
async fn a_wrong_verifier_or_a_reused_code_is_refused_at_redeem() {
    let backend = backend(&[]);
    let origin = &backend.origin;
    let flow = Flow::start(&client(), origin).await.unwrap();
    let state = state_of(flow.authorize_url());
    let port = flow.port();
    // The code is read off the redirect before it reaches the listener, as someone watching the browser could.
    let location = backend_callback(origin, &state, "discord-code").await;
    let code = query_value(&location, "code");
    assert_eq!(location.port(), Some(port));
    let wrong = client()
        .redeem_sign_in(
            origin,
            &state,
            &code,
            "WRONGverifierWRONGverifierWRONGverifierWRON",
        )
        .await;
    assert!(
        matches!(&wrong, Err(ApiError::Refused { status: 400, code }) if code == "invalid"),
        "{wrong:?}"
    );
    // The code was spent by that attempt, so even the right flow cannot redeem it now.
    let finishing = tokio::spawn(async move { flow.finish(&client(), DEADLINE).await });
    browser().get(location).send().await.unwrap();
    assert!(matches!(
        finishing.await.unwrap(),
        Err(SignInError::Api(ApiError::Refused { status: 400, .. }))
    ));
}

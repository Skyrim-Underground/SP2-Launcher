// SPDX-License-Identifier: AGPL-3.0-or-later
//! The HTTP client for a server's /api/v1/ routes and for the directory files.

use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::{StatusCode, redirect};
use serde::{Deserialize, Serialize};
use url::Url;

/// The API major versions this launcher speaks; a backend outside this list cannot be used.
pub const KNOWN_API_VERSIONS: &[u32] = &[1];
const JSON_MAX_BYTES: usize = 64 * 1024;
const MANIFEST_MAX_BYTES: usize = crate::manifest::MAX_BYTES;
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 5;
const SERVER_ROUTE: &str = "/api/v1/server";
const STATUS_ROUTE: &str = "/api/v1/status";
const LOGIN_START_ROUTE: &str = "/api/v1/users/login-discord/start";
const LOGIN_REDEEM_ROUTE: &str = "/api/v1/users/login-discord/redeem";
const ME_ROUTE: &str = "/api/v1/users/me";
const LOGOUT_ROUTE: &str = "/api/v1/users/logout";
const PLAY_ROUTE: &str = "/api/v1/play";
const JSON_TYPE: &str = "application/json";

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("the HTTP client could not be built: {0}")]
    Client(String),
    #[error("not a valid URL")]
    Url,
    #[error("the server could not be reached: {0}")]
    Network(String),
    #[error("the server redirected to another origin")]
    Redirect,
    #[error("the server answered with status {0}")]
    Status(u16),
    #[error("the answer of the server is not valid: {0}")]
    Body(String),
    #[error("the answer of the server is larger than {0} bytes")]
    TooLarge(usize),
    #[error("the server points at a manifest outside its own origin")]
    ForeignManifest,
    #[error("the sign-in for this server is no longer valid; sign in again")]
    Unauthorized,
    #[error("{}", refusal_text(.code))]
    Refused { status: u16, code: String },
}

/// What a refusal code from a backend means to the player, in plain words.
pub fn refusal_text(code: &str) -> String {
    match code {
        "banned" => "you are banned from this server".to_string(),
        "not_member" => "you are not a member of this server's Discord".to_string(),
        "missing_role" => {
            "your Discord account has no role that allows playing on this server".to_string()
        }
        "discord_unavailable" => {
            "the server cannot reach Discord right now; try again later".to_string()
        }
        "server_offline" => "the game server is offline".to_string(),
        "server_full" => "the game server is full".to_string(),
        "denied" => "the sign-in was not completed in the browser".to_string(),
        "invalid_code" => "Discord did not accept the sign-in; try again".to_string(),
        "creation_locked" => "the server is not taking new accounts right now".to_string(),
        "busy" | "too_many_pending" | "too_many_requests" => {
            "the server is busy; wait a moment and try again".to_string()
        }
        other => format!("the server refused the request ({other})"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartRequest<'a> {
    challenge: &'a str,
    port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartResponse {
    pub state: String,
    pub authorize_url: String,
    pub expires_in: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RedeemRequest<'a> {
    state: &'a str,
    code: &'a str,
    verifier: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedeemResponse {
    pub session: String,
    pub expires_at: u64,
    pub display_name: String,
}

/// Whether the signed-in account may play, as the backend last saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Ok,
    NotMember,
    MissingRole,
    Banned,
    DiscordUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Me {
    pub display_name: String,
    pub access: Access,
    #[serde(default)]
    pub ban_until: Option<u64>,
    #[serde(default)]
    pub ban_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameAddress {
    pub host: String,
    pub port: u16,
}

/// The answer to Play: a single-use token and where the game connects with it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlayGrant {
    pub play_token: String,
    pub expires_at: u64,
    pub server: GameAddress,
}

/// The one field every refusal carries; the rest of such a body is the server's own.
#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerInfo {
    pub name: String,
    pub description: String,
    pub website: String,
    pub discord_invite: String,
    pub protocol_id: String,
    pub game_runtime: String,
    pub api_version: u32,
    pub manifest_url: String,
}

/// Only the field the gate needs, read before the full shape, because another major version may change the rest.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiVersionOnly {
    api_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerCheck {
    Ok { info: ServerInfo },
    LauncherTooOld { api_version: u32 },
    ServerTooOld { api_version: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Status {
    pub online: bool,
    pub players: u32,
    pub max_players: u32,
    pub sign_in: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Known,
    LauncherTooOld,
    ServerTooOld,
}

pub fn gate(api_version: u32) -> Gate {
    if KNOWN_API_VERSIONS.contains(&api_version) {
        Gate::Known
    } else if KNOWN_API_VERSIONS.iter().all(|known| api_version > *known) {
        Gate::LauncherTooOld
    } else {
        Gate::ServerTooOld
    }
}

#[derive(Debug, thiserror::Error)]
#[error("redirect to another origin")]
struct CrossOriginRedirect;

/// Redirects stay on the origin of the first request, so a server cannot send the launcher elsewhere.
fn redirect_policy() -> redirect::Policy {
    redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if !same_origin {
            attempt.error(CrossOriginRedirect)
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

fn network(error: reqwest::Error) -> ApiError {
    if error.is_redirect() {
        ApiError::Redirect
    } else {
        ApiError::Network(error.without_url().to_string())
    }
}

fn body(error: serde_json::Error) -> ApiError {
    ApiError::Body(error.to_string())
}

fn route(origin: &Url, path: &str) -> Result<Url, ApiError> {
    origin.join(path).map_err(|_| ApiError::Url)
}

#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
}

impl ApiClient {
    /// A client that answers within 10 s, ignores any proxy in the environment and never leaves an origin on a redirect.
    pub fn new(user_agent: &str) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .no_proxy()
            .redirect(redirect_policy())
            .build()
            .map_err(|error| ApiError::Client(error.without_url().to_string()))?;
        Ok(Self { http })
    }

    pub async fn server(&self, origin: &Url) -> Result<ServerCheck, ApiError> {
        let bytes = self
            .get_bytes(&route(origin, SERVER_ROUTE)?, JSON_MAX_BYTES)
            .await?;
        let version: ApiVersionOnly = serde_json::from_slice(&bytes).map_err(body)?;
        Ok(match gate(version.api_version) {
            Gate::LauncherTooOld => ServerCheck::LauncherTooOld {
                api_version: version.api_version,
            },
            Gate::ServerTooOld => ServerCheck::ServerTooOld {
                api_version: version.api_version,
            },
            Gate::Known => ServerCheck::Ok {
                info: serde_json::from_slice(&bytes).map_err(body)?,
            },
        })
    }

    pub async fn status(&self, origin: &Url) -> Result<Status, ApiError> {
        let bytes = self
            .get_bytes(&route(origin, STATUS_ROUTE)?, JSON_MAX_BYTES)
            .await?;
        serde_json::from_slice(&bytes).map_err(body)
    }

    /// The manifest bytes as served; `manifest_url` comes from /api/v1/server and must stay on that origin.
    pub async fn manifest(&self, origin: &Url, manifest_url: &str) -> Result<Vec<u8>, ApiError> {
        let url = route(origin, manifest_url)?;
        if url.origin() != origin.origin() {
            return Err(ApiError::ForeignManifest);
        }
        self.get_bytes(&url, MANIFEST_MAX_BYTES).await
    }

    pub async fn get_bytes(&self, url: &Url, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
        let response = self.http.get(url.clone()).send().await.map_err(network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::Status(status.as_u16()));
        }
        read_body(response, max_bytes).await
    }

    /// Begins a sign-in: the backend learns the challenge and the port the browser is sent back to.
    pub async fn start_sign_in(
        &self,
        origin: &Url,
        challenge: &str,
        port: u16,
    ) -> Result<StartResponse, ApiError> {
        let bytes = self
            .post(
                &route(origin, LOGIN_START_ROUTE)?,
                &StartRequest { challenge, port },
                None,
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(body)
    }

    /// Trades the login code the browser brought back, with the verifier, for the session.
    pub async fn redeem_sign_in(
        &self,
        origin: &Url,
        state: &str,
        code: &str,
        verifier: &str,
    ) -> Result<RedeemResponse, ApiError> {
        let bytes = self
            .post(
                &route(origin, LOGIN_REDEEM_ROUTE)?,
                &RedeemRequest {
                    state,
                    code,
                    verifier,
                },
                None,
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(body)
    }

    pub async fn me(&self, origin: &Url, session: &str) -> Result<Me, ApiError> {
        let request = self.http.get(route(origin, ME_ROUTE)?).bearer_auth(session);
        let bytes = self.send(request).await?;
        serde_json::from_slice(&bytes).map_err(body)
    }

    pub async fn logout(&self, origin: &Url, session: &str) -> Result<(), ApiError> {
        let request = self
            .http
            .post(route(origin, LOGOUT_ROUTE)?)
            .bearer_auth(session);
        self.send(request).await?;
        Ok(())
    }

    /// Asks for a play token; a refusal names its reason in the error.
    pub async fn play(&self, origin: &Url, session: &str) -> Result<PlayGrant, ApiError> {
        let bytes = self
            .post(&route(origin, PLAY_ROUTE)?, &EmptyBody {}, Some(session))
            .await?;
        serde_json::from_slice(&bytes).map_err(body)
    }

    /// Sends a JSON body; the session, when given, travels in the Authorization header of this one request to this one URL.
    async fn post<B: Serialize>(
        &self,
        url: &Url,
        payload: &B,
        session: Option<&str>,
    ) -> Result<Vec<u8>, ApiError> {
        let mut request = self
            .http
            .post(url.clone())
            .header(CONTENT_TYPE, JSON_TYPE)
            .body(serde_json::to_vec(payload).map_err(body)?);
        if let Some(session) = session {
            request = request.bearer_auth(session);
        }
        self.send(request).await
    }

    /// The body of a successful answer; a refusal is typed from the {error} the server sends with it.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Vec<u8>, ApiError> {
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        let bytes = read_body(response, JSON_MAX_BYTES).await?;
        if status.is_success() {
            return Ok(bytes);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        match serde_json::from_slice::<ErrorBody>(&bytes) {
            Ok(refusal) => Err(ApiError::Refused {
                status: status.as_u16(),
                code: refusal.error,
            }),
            Err(_) => Err(ApiError::Status(status.as_u16())),
        }
    }
}

#[derive(Serialize)]
struct EmptyBody {}

async fn read_body(mut response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(ApiError::TooLarge(max_bytes));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > max_bytes {
            return Err(ApiError::TooLarge(max_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{CannedServer, server_json};

    fn client() -> ApiClient {
        ApiClient::new("sug-launcher-tests/0").unwrap()
    }

    #[test]
    fn the_gate_knows_version_one_only() {
        assert_eq!(gate(1), Gate::Known);
        assert_eq!(gate(2), Gate::LauncherTooOld);
        assert_eq!(gate(0), Gate::ServerTooOld);
    }

    #[tokio::test]
    async fn api_version_two_needs_a_newer_launcher() {
        let server = CannedServer::start();
        server.json(SERVER_ROUTE, 200, &server_json("Future", 2));
        let check = client().server(server.origin()).await.unwrap();
        assert_eq!(check, ServerCheck::LauncherTooOld { api_version: 2 });
    }

    #[tokio::test]
    async fn api_version_zero_is_too_old() {
        let server = CannedServer::start();
        server.json(SERVER_ROUTE, 200, &server_json("Ancient", 0));
        let check = client().server(server.origin()).await.unwrap();
        assert_eq!(check, ServerCheck::ServerTooOld { api_version: 0 });
    }

    #[tokio::test]
    async fn api_version_one_gives_the_server_info() {
        let server = CannedServer::start();
        server.json(SERVER_ROUTE, 200, &server_json("Known", 1));
        let check = client().server(server.origin()).await.unwrap();
        let ServerCheck::Ok { info } = check else {
            panic!("expected Ok, got {check:?}");
        };
        assert_eq!(info.name, "Known");
        assert_eq!(info.api_version, 1);
        assert_eq!(info.manifest_url, "/api/v1/manifest");
    }

    #[tokio::test]
    async fn an_unknown_field_in_a_known_version_is_refused() {
        let server = CannedServer::start();
        let mut text = server_json("Odd", 1);
        text.insert_str(1, r#""extra":true,"#);
        server.json(SERVER_ROUTE, 200, &text);
        assert!(matches!(
            client().server(server.origin()).await,
            Err(ApiError::Body(_))
        ));
    }

    #[tokio::test]
    async fn errors_carry_the_status_or_the_network_failure() {
        let server = CannedServer::start();
        server.json(SERVER_ROUTE, 503, r#"{"error":"down"}"#);
        assert!(matches!(
            client().server(server.origin()).await,
            Err(ApiError::Status(503))
        ));
        let unreachable = Url::parse("http://127.0.0.1:1/").unwrap();
        assert!(matches!(
            client().status(&unreachable).await,
            Err(ApiError::Network(_))
        ));
    }

    #[tokio::test]
    async fn status_is_typed() {
        let server = CannedServer::start();
        server.json(
            STATUS_ROUTE,
            200,
            r#"{"online":true,"players":3,"maxPlayers":64,"signIn":"unavailable"}"#,
        );
        let status = client().status(server.origin()).await.unwrap();
        assert_eq!(
            status,
            Status {
                online: true,
                players: 3,
                max_players: 64,
                sign_in: "unavailable".to_string()
            }
        );
    }

    #[tokio::test]
    async fn bodies_are_capped_whether_or_not_their_size_is_announced() {
        let server = CannedServer::start();
        let url = server.origin().join("/big").unwrap();
        server.bytes("/big", vec![b'x'; 33]);
        assert!(matches!(
            client().get_bytes(&url, 32).await,
            Err(ApiError::TooLarge(32))
        ));
        server.bytes_unsized("/big", vec![b'x'; 33]);
        assert!(matches!(
            client().get_bytes(&url, 32).await,
            Err(ApiError::TooLarge(32))
        ));
        server.bytes("/big", vec![b'x'; 32]);
        assert_eq!(client().get_bytes(&url, 32).await.unwrap().len(), 32);
    }

    #[tokio::test]
    async fn the_manifest_is_fetched_from_its_relative_route_only() {
        let server = CannedServer::start();
        server.bytes("/api/v1/manifest", b"{\"schemaVersion\":1}".to_vec());
        let bytes = client()
            .manifest(server.origin(), "/api/v1/manifest")
            .await
            .unwrap();
        assert_eq!(bytes, b"{\"schemaVersion\":1}");
        let other = CannedServer::start();
        other.bytes("/api/v1/manifest", b"{}".to_vec());
        let elsewhere = other.origin().join("/api/v1/manifest").unwrap();
        assert!(matches!(
            client().manifest(server.origin(), elsewhere.as_str()).await,
            Err(ApiError::ForeignManifest)
        ));
        assert!(other.hits().is_empty());
        server.bytes("/api/v1/manifest", vec![b' '; MANIFEST_MAX_BYTES + 1]);
        assert!(matches!(
            client().manifest(server.origin(), "/api/v1/manifest").await,
            Err(ApiError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn the_account_routes_are_typed_and_refusals_carry_their_code() {
        let server = CannedServer::start();
        let session = "sugs_TESTtestTESTtestTESTtestTESTtestTESTtes";
        server.json(
            ME_ROUTE,
            200,
            r#"{"displayName":"Tester","access":"banned","banUntil":1760000000000,"banReason":"test ban"}"#,
        );
        let me = client().me(server.origin(), session).await.unwrap();
        assert_eq!(
            me,
            Me {
                display_name: "Tester".to_string(),
                access: Access::Banned,
                ban_until: Some(1_760_000_000_000),
                ban_reason: Some("test ban".to_string()),
            }
        );
        server.json(ME_ROUTE, 200, r#"{"displayName":"Tester","access":"ok"}"#);
        let me = client().me(server.origin(), session).await.unwrap();
        assert_eq!(me.access, Access::Ok);
        assert_eq!(me.ban_until, None);
        server.json(ME_ROUTE, 200, r#"{"displayName":"Tester","access":"vip"}"#);
        assert!(matches!(
            client().me(server.origin(), session).await,
            Err(ApiError::Body(_))
        ));
        server.json(ME_ROUTE, 401, r#"{"error":"unauthorized"}"#);
        assert!(matches!(
            client().me(server.origin(), session).await,
            Err(ApiError::Unauthorized)
        ));
        server.json(PLAY_ROUTE, 403, r#"{"error":"banned"}"#);
        let refused = client().play(server.origin(), session).await.unwrap_err();
        assert!(matches!(
            &refused,
            ApiError::Refused { status: 403, code } if code == "banned"
        ));
        assert_eq!(refused.to_string(), "you are banned from this server");
        server.json(PLAY_ROUTE, 503, r#"{"error":"server_full"}"#);
        assert_eq!(
            client()
                .play(server.origin(), session)
                .await
                .unwrap_err()
                .to_string(),
            "the game server is full"
        );
        server.json(PLAY_ROUTE, 500, "not json");
        assert!(matches!(
            client().play(server.origin(), session).await,
            Err(ApiError::Status(500))
        ));
        server.json(
            PLAY_ROUTE,
            200,
            r#"{"playToken":"sugp_TESTtestTESTtestTESTtestTESTtestTESTtes","expiresAt":1760000300000,"server":{"host":"game.example.org","port":7777}}"#,
        );
        let grant = client().play(server.origin(), session).await.unwrap();
        assert_eq!(grant.server.host, "game.example.org");
        assert_eq!(grant.server.port, 7777);
        assert!(grant.play_token.starts_with("sugp_"));
        server.json(LOGOUT_ROUTE, 204, "");
        client().logout(server.origin(), session).await.unwrap();
        assert_eq!(
            refusal_text("something_else"),
            "the server refused the request (something_else)"
        );
    }

    #[tokio::test]
    async fn the_sign_in_routes_are_typed() {
        let server = CannedServer::start();
        server.json(
            LOGIN_START_ROUTE,
            200,
            r#"{"state":"abc","authorizeUrl":"https://discord.com/oauth2/authorize?state=abc","expiresIn":600}"#,
        );
        let started = client()
            .start_sign_in(server.origin(), "challenge", 4321)
            .await
            .unwrap();
        assert_eq!(started.state, "abc");
        assert_eq!(started.expires_in, 600);
        server.json(LOGIN_START_ROUTE, 503, r#"{"error":"busy"}"#);
        assert_eq!(
            client()
                .start_sign_in(server.origin(), "challenge", 4321)
                .await
                .unwrap_err()
                .to_string(),
            "the server is busy; wait a moment and try again"
        );
        server.json(
            LOGIN_REDEEM_ROUTE,
            200,
            r#"{"session":"sugs_TESTtestTESTtestTESTtestTESTtestTESTtes","expiresAt":1762592000000,"displayName":"Tester"}"#,
        );
        let redeemed = client()
            .redeem_sign_in(server.origin(), "abc", "code", "verifier")
            .await
            .unwrap();
        assert_eq!(redeemed.display_name, "Tester");
        assert!(redeemed.session.starts_with("sugs_"));
        server.json(LOGIN_REDEEM_ROUTE, 400, r#"{"error":"invalid"}"#);
        assert!(matches!(
            client()
                .redeem_sign_in(server.origin(), "abc", "code", "verifier")
                .await,
            Err(ApiError::Refused { status: 400, code }) if code == "invalid"
        ));
    }

    #[tokio::test]
    async fn redirects_are_followed_on_the_same_origin_only() {
        let server = CannedServer::start();
        server.redirect(SERVER_ROUTE, "/moved");
        server.json("/moved", 200, &server_json("Moved", 1));
        assert!(matches!(
            client().server(server.origin()).await,
            Ok(ServerCheck::Ok { .. })
        ));
        let other = CannedServer::start();
        other.json(SERVER_ROUTE, 200, &server_json("Other", 1));
        server.redirect(
            SERVER_ROUTE,
            other.origin().join(SERVER_ROUTE).unwrap().as_str(),
        );
        assert!(matches!(
            client().server(server.origin()).await,
            Err(ApiError::Redirect)
        ));
        assert!(other.hits().is_empty());
    }
}

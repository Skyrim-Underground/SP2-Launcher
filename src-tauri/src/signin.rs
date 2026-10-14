// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sign-in to one server: a PKCE pair, a loopback listener for the browser's return, and the redeem that yields the session.

use std::io;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use url::Url;

use crate::api::{ApiClient, ApiError, RedeemResponse, refusal_text};
use crate::secrets::{SecretError, random_bytes};

/// How long the browser has to come back once the sign-in was started.
pub const DEADLINE: Duration = Duration::from_secs(300);
/// How long one connection may take to send its request before it is dropped for the next one.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// The only address the launcher opens for a sign-in; anything else a server sends is refused unopened.
pub const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
/// The path the backend sends the browser back to on the loopback listener.
pub const CALLBACK_PATH: &str = "/callback";
const AUTHORIZE_HOST: &str = "discord.com";
const AUTHORIZE_PATH: &str = "/oauth2/authorize";
const VERIFIER_BYTES: usize = 32;
const HEAD_MAX_BYTES: usize = 8 * 1024;
const DONE_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Signed in</title></head><body><h1>Signed in</h1><p>You can close this tab and return to the launcher.</p></body></html>";
const REFUSED_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Sign-in refused</title></head><body><h1>Sign-in refused</h1><p>The launcher shows why. You can close this tab.</p></body></html>";
const IGNORED: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("the listener for the browser's return could not be opened: {0}")]
    Listen(io::Error),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("the server sent a sign-in address that is not Discord's, so it was not opened")]
    AuthorizeUrl,
    #[error("the sign-in was not finished in the browser within {0} minutes")]
    Timeout(u64),
    #[error("the sign-in was refused: {}", refusal_text(.0))]
    Refused(String),
    #[error("the browser came back without a sign-in code")]
    NoCode,
    #[error("the browser's return could not be received: {0}")]
    Accept(io::Error),
}

/// A verifier of 32 random bytes in base64url and its S256 challenge, the base64url SHA-256 of that text.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn new_pkce() -> Result<Pkce, SecretError> {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(VERIFIER_BYTES)?);
    let challenge = challenge_of(&verifier);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

pub fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Accepts Discord's authorize address and nothing else: https, the host discord.com on its default port, the exact path.
pub fn check_authorize_url(text: &str) -> Result<Url, SignInError> {
    let url = Url::parse(text).map_err(|_| SignInError::AuthorizeUrl)?;
    let discord = url.scheme() == "https"
        && url.host_str() == Some(AUTHORIZE_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == AUTHORIZE_PATH;
    if discord {
        Ok(url)
    } else {
        Err(SignInError::AuthorizeUrl)
    }
}

/// What the browser brought back for the flow's own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Code(String),
    /// The backend's reason for refusing, as it named it in the redirect.
    Error(String),
    Empty,
}

/// The callback for the flow's own state, or None when the request is for another path or another state.
fn parse_callback(target: &str, state: &str) -> Option<Callback> {
    let url = Url::parse("http://127.0.0.1").ok()?.join(target).ok()?;
    if url.path() != CALLBACK_PATH {
        return None;
    }
    let mut own = false;
    let mut code = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match &*key {
            "state" => own = value == state,
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if !own {
        return None;
    }
    Some(match (code, error) {
        (Some(code), _) => Callback::Code(code),
        (None, Some(error)) => Callback::Error(error),
        (None, None) => Callback::Empty,
    })
}

/// The request target of a GET, read from the head of the connection; anything else is None.
async fn read_target(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut head = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
        if head.windows(4).any(|window| window == b"\r\n\r\n") || head.len() >= HEAD_MAX_BYTES {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    Ok(match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => Some(target.to_string()),
        _ => None,
    })
}

/// The browser already has what it needs, so a failure to answer it changes nothing for the flow.
async fn respond(mut stream: TcpStream, response: &str) {
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn page(html: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nContent-Security-Policy: default-src 'none'\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    )
}

/// A listener on 127.0.0.1 with a port the system chose, open for one browser return.
pub struct Listener {
    listener: TcpListener,
    port: u16,
}

impl Listener {
    pub async fn bind() -> Result<Self, SignInError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(SignInError::Listen)?;
        let port = listener.local_addr().map_err(SignInError::Listen)?.port();
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Answers the first request carrying `state` and closes; any other request gets a 404 and is ignored.
    /// A connection that sends no request within `HEAD_TIMEOUT` is dropped, so it cannot hold up the browser's return.
    /// Past the deadline the listener closes unanswered, and the port is free again either way.
    pub async fn wait(self, state: &str, deadline: Duration) -> Result<Callback, SignInError> {
        let minutes = deadline.as_secs().div_ceil(60).max(1);
        timeout(deadline, self.accept_own(state))
            .await
            .unwrap_or_else(|_| Err(SignInError::Timeout(minutes)))
    }

    async fn accept_own(self, state: &str) -> Result<Callback, SignInError> {
        loop {
            let (mut stream, _) = self.listener.accept().await.map_err(SignInError::Accept)?;
            // A browser sends its request as soon as it connects; a connection that does not is not the browser's return.
            let Ok(head) = timeout(HEAD_TIMEOUT, read_target(&mut stream)).await else {
                continue;
            };
            let callback = head
                .ok()
                .flatten()
                .and_then(|target| parse_callback(&target, state));
            let Some(callback) = callback else {
                respond(stream, IGNORED).await;
                continue;
            };
            let html = match callback {
                Callback::Code(_) => DONE_PAGE,
                Callback::Error(_) | Callback::Empty => REFUSED_PAGE,
            };
            respond(stream, &page(html)).await;
            return Ok(callback);
        }
    }
}

/// One sign-in from the start request to the session; the caller opens the browser at `authorize_url`.
pub struct Flow {
    origin: Url,
    state: String,
    verifier: String,
    listener: Listener,
    authorize_url: Url,
}

impl Flow {
    /// Makes the PKCE pair, opens the listener, registers both with the backend and checks the address it answered with.
    pub async fn start(client: &ApiClient, origin: &Url) -> Result<Self, SignInError> {
        let pkce = new_pkce()?;
        let listener = Listener::bind().await?;
        let started = client
            .start_sign_in(origin, &pkce.challenge, listener.port())
            .await?;
        let authorize_url = check_authorize_url(&started.authorize_url)?;
        Ok(Self {
            origin: origin.clone(),
            state: started.state,
            verifier: pkce.verifier,
            listener,
            authorize_url,
        })
    }

    pub fn authorize_url(&self) -> &Url {
        &self.authorize_url
    }

    pub fn port(&self) -> u16 {
        self.listener.port()
    }

    /// Waits for the browser, then redeems the code with the verifier; the listener is closed whichever way this ends.
    pub async fn finish(
        self,
        client: &ApiClient,
        deadline: Duration,
    ) -> Result<RedeemResponse, SignInError> {
        let Self {
            origin,
            state,
            verifier,
            listener,
            ..
        } = self;
        match listener.wait(&state, deadline).await? {
            Callback::Code(code) => Ok(client
                .redeem_sign_in(&origin, &state, &code, &verifier)
                .await?),
            Callback::Error(error) => Err(SignInError::Refused(error)),
            Callback::Empty => Err(SignInError::NoCode),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream as StdStream;
    use std::time::Instant;

    use super::*;

    const STATE: &str = "OWNstateOWNstateOWNstateOWNstateOWNstateOWN";

    fn is_base64url(text: &str) -> bool {
        text.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    }

    /// Sends one GET to the listener like a browser would and returns the status line and body.
    fn browse(port: u16, target: &str) -> String {
        let mut stream = StdStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    #[test]
    fn the_challenge_is_the_rfc_7636_vector_and_verifiers_are_fresh() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let first = new_pkce().unwrap();
        let second = new_pkce().unwrap();
        assert_eq!(first.verifier.len(), 43);
        assert_eq!(first.challenge.len(), 43);
        assert!(is_base64url(&first.verifier));
        assert!(is_base64url(&first.challenge));
        assert_eq!(first.challenge, challenge_of(&first.verifier));
        assert_ne!(first.verifier, second.verifier);
    }

    #[test]
    fn only_discords_authorize_address_is_opened() {
        for accepted in [
            "https://discord.com/oauth2/authorize?client_id=1&state=abc&code_challenge=x",
            "https://discord.com/oauth2/authorize",
        ] {
            let url = check_authorize_url(accepted).unwrap();
            assert!(url.as_str().starts_with(AUTHORIZE_URL), "{accepted}");
        }
        for refused in [
            "http://discord.com/oauth2/authorize?client_id=1",
            "https://discord.com:8443/oauth2/authorize",
            "https://user@discord.com/oauth2/authorize",
            "https://discord.com.evil.test/oauth2/authorize",
            "https://evil.test/oauth2/authorize",
            "https://discord.com/oauth2/authorizex",
            "https://discord.com/oauth2/authorize/",
            "https://discord.com/oauth2/authorize/../token",
            "https://discord.com/",
            "discord.com/oauth2/authorize",
            "",
        ] {
            assert!(
                matches!(check_authorize_url(refused), Err(SignInError::AuthorizeUrl)),
                "{refused}"
            );
        }
    }

    #[test]
    fn a_callback_counts_only_on_the_callback_path_with_the_own_state() {
        assert_eq!(
            parse_callback("/callback?state=abc&code=xyz", "abc"),
            Some(Callback::Code("xyz".to_string()))
        );
        assert_eq!(
            parse_callback("/callback?code=xyz&state=abc", "abc"),
            Some(Callback::Code("xyz".to_string()))
        );
        assert_eq!(
            parse_callback("/callback?state=abc&error=not_member", "abc"),
            Some(Callback::Error("not_member".to_string()))
        );
        assert_eq!(
            parse_callback("/callback?state=abc", "abc"),
            Some(Callback::Empty)
        );
        assert_eq!(
            parse_callback("/callback?state=other&code=xyz", "abc"),
            None
        );
        assert_eq!(parse_callback("/callback?code=xyz", "abc"), None);
        assert_eq!(parse_callback("/other?state=abc&code=xyz", "abc"), None);
        assert_eq!(
            parse_callback("/callback/x?state=abc&code=xyz", "abc"),
            None
        );
        assert_eq!(parse_callback("not a target", "abc"), None);
        assert_eq!(
            SignInError::Refused("not_member".to_string()).to_string(),
            "the sign-in was refused: you are not a member of this server's Discord"
        );
    }

    #[tokio::test]
    async fn a_foreign_state_is_ignored_and_the_own_state_ends_the_wait() {
        let listener = Listener::bind().await.unwrap();
        let port = listener.port();
        let waiting =
            tokio::spawn(async move { listener.wait(STATE, Duration::from_secs(20)).await });
        let foreign = tokio::task::spawn_blocking(move || {
            browse(port, "/callback?state=FOREIGNstate&code=stolen")
        })
        .await
        .unwrap();
        assert!(foreign.starts_with("HTTP/1.1 404"), "{foreign}");
        let elsewhere = tokio::task::spawn_blocking(move || browse(port, "/favicon.ico"))
            .await
            .unwrap();
        assert!(elsewhere.starts_with("HTTP/1.1 404"), "{elsewhere}");
        assert!(
            !waiting.is_finished(),
            "a foreign state must not end the wait"
        );
        let own = tokio::task::spawn_blocking(move || {
            browse(
                port,
                &format!("/callback?state={STATE}&code=the-login-code"),
            )
        })
        .await
        .unwrap();
        assert!(own.starts_with("HTTP/1.1 200"), "{own}");
        assert!(own.contains("default-src 'none'"));
        assert!(own.contains("You can close this tab"));
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            Callback::Code("the-login-code".to_string())
        );
        // The listener went with the wait, so the port is free for the next sign-in.
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).unwrap());
    }

    #[tokio::test]
    async fn an_error_from_the_backend_ends_the_wait_with_its_reason() {
        let listener = Listener::bind().await.unwrap();
        let port = listener.port();
        let waiting =
            tokio::spawn(async move { listener.wait(STATE, Duration::from_secs(20)).await });
        let answer = tokio::task::spawn_blocking(move || {
            browse(port, &format!("/callback?state={STATE}&error=missing_role"))
        })
        .await
        .unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains("Sign-in refused"));
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            Callback::Error("missing_role".to_string())
        );
    }

    #[tokio::test]
    async fn a_connection_that_sends_nothing_cannot_hold_up_the_browser() {
        let listener = Listener::bind().await.unwrap();
        let port = listener.port();
        let started = Instant::now();
        let waiting =
            tokio::spawn(async move { listener.wait(STATE, Duration::from_secs(60)).await });
        // Connected first and kept open without sending a byte, so it sits in front of the browser's request.
        let silent = StdStream::connect(("127.0.0.1", port)).unwrap();
        let own = tokio::task::spawn_blocking(move || {
            browse(
                port,
                &format!("/callback?state={STATE}&code=the-login-code"),
            )
        })
        .await
        .unwrap();
        assert!(own.starts_with("HTTP/1.1 200"), "{own}");
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            Callback::Code("the-login-code".to_string())
        );
        // The silent connection was still open, so only the bound can have let the browser's request through.
        assert!(started.elapsed() >= HEAD_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(60));
        drop(silent);
    }

    #[tokio::test]
    async fn the_listener_closes_after_the_deadline_and_the_port_is_free() {
        assert_eq!(DEADLINE, Duration::from_secs(300));
        let listener = Listener::bind().await.unwrap();
        let port = listener.port();
        let started = Instant::now();
        let ended = listener.wait(STATE, Duration::from_millis(300)).await;
        assert!(matches!(ended, Err(SignInError::Timeout(1))), "{ended:?}");
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(10));
        drop(std::net::TcpListener::bind(("127.0.0.1", port)).unwrap());
        assert_eq!(
            SignInError::Timeout(5).to_string(),
            "the sign-in was not finished in the browser within 5 minutes"
        );
    }
}

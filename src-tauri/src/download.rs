// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streamed downloads into a .part file: resumed with Range, hashed while streaming, capped at the size the manifest states.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::header::{CONTENT_RANGE, LOCATION, RANGE};
use reqwest::{StatusCode, redirect};
use sha2::{Digest, Sha256};
use url::Url;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
const READ_BUFFER: usize = 64 * 1024;
pub const PART_SUFFIX: &str = ".part";

/// Shared between the UI's Cancel button and the install running in the background.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("the HTTP client could not be built: {0}")]
    Client(String),
    #[error("the install was cancelled")]
    Cancelled,
    #[error("the download led to {host}, which the manifest does not list as an origin")]
    OffOrigin { host: String },
    #[error("the download was redirected too many times")]
    TooManyRedirects,
    #[error("the download failed: {0}")]
    Network(String),
    #[error("the host answered with status {0}")]
    Status(u16),
    #[error("the host sent more than the {size} bytes the manifest states")]
    SizeExceeded { size: u64 },
    #[error("the download stopped after {got} of {size} bytes")]
    Incomplete { got: u64, size: u64 },
    #[error("the host did not resume where the download had stopped")]
    BadResume,
    #[error("the downloaded file does not match the sha256 the manifest states")]
    HashMismatch,
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// One file to fetch: where from, which origins may serve it, where its .part lives, and what it must turn out to be.
pub struct Request<'a> {
    pub url: &'a Url,
    pub origins: &'a [Url],
    pub part: &'a Path,
    pub size: u64,
    pub sha256: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// Bytes already in the .part file before this call.
    pub resumed_from: u64,
    /// Bytes this call fetched.
    pub bytes: u64,
}

/// Bytes done so far, including those resumed from the .part file.
pub type Progress<'a> = dyn FnMut(u64) + Send + 'a;

pub fn on_listed_origin(url: &Url, origins: &[Url]) -> bool {
    origins.iter().any(|origin| origin.origin() == url.origin())
}

pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256_of_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn host_of(url: &Url) -> String {
    url.host_str().unwrap_or_default().to_string()
}

fn network(error: reqwest::Error) -> DownloadError {
    DownloadError::Network(error.without_url().to_string())
}

pub fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The offset a 206 answer starts at, from "bytes <start>-<end>/<total>".
fn content_range_start(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// The .part file as a download starts: how much is there and the hash of it so far.
struct Part {
    length: u64,
    hasher: Sha256,
}

impl Part {
    fn open(path: &Path, size: u64) -> io::Result<Self> {
        let mut hasher = Sha256::new();
        let length = match fs::metadata(path) {
            Ok(meta) if meta.len() <= size => meta.len(),
            Ok(_) => {
                remove_if_present(path)?;
                0
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if length > 0 {
            let mut file = File::open(path)?;
            let mut buffer = vec![0u8; READ_BUFFER];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        }
        Ok(Self { length, hasher })
    }

    fn fresh() -> Self {
        Self {
            length: 0,
            hasher: Sha256::new(),
        }
    }
}

#[derive(Clone)]
pub struct Downloader {
    http: reqwest::Client,
}

impl Downloader {
    /// Redirects are not followed by the client, so every hop passes the origin check in `open`.
    pub fn new(user_agent: &str) -> Result<Self, DownloadError> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|error| DownloadError::Client(error.without_url().to_string()))?;
        Ok(Self { http })
    }

    /// Fetches into the .part file and leaves it complete and verified; an unfinished part is kept for a resume, a wrong one removed.
    pub async fn fetch(
        &self,
        request: &Request<'_>,
        cancel: &Cancel,
        progress: &mut Progress<'_>,
    ) -> Result<Received, DownloadError> {
        if !on_listed_origin(request.url, request.origins) {
            return Err(DownloadError::OffOrigin {
                host: host_of(request.url),
            });
        }
        let mut part = Part::open(request.part, request.size)?;
        // A part that already holds every byte is only checked; one that holds the wrong bytes starts over.
        if part.length == request.size && request.size > 0 {
            if hex_lower(&part.hasher.clone().finalize()) == request.sha256 {
                progress(request.size);
                return Ok(Received {
                    resumed_from: request.size,
                    bytes: 0,
                });
            }
            remove_if_present(request.part)?;
            part = Part::fresh();
        }
        let resumed = part.length > 0;
        let first = self.transfer(request, part, cancel, progress).await;
        // A stale part from another version of the file fails the hash, so it is dropped and the file fetched whole.
        if resumed && matches!(first, Err(DownloadError::HashMismatch)) {
            return self
                .transfer(request, Part::fresh(), cancel, progress)
                .await;
        }
        first
    }

    async fn transfer(
        &self,
        request: &Request<'_>,
        part: Part,
        cancel: &Cancel,
        progress: &mut Progress<'_>,
    ) -> Result<Received, DownloadError> {
        let Part {
            mut length,
            mut hasher,
        } = part;
        let response = self.open(request, length).await?;
        let status = response.status();
        if length > 0 {
            match status {
                StatusCode::PARTIAL_CONTENT => {
                    if content_range_start(&response) != Some(length) {
                        remove_if_present(request.part)?;
                        return Err(DownloadError::BadResume);
                    }
                }
                StatusCode::OK => {
                    // The host ignored the range, so the whole file arrives and the part starts over.
                    remove_if_present(request.part)?;
                    length = 0;
                    hasher = Sha256::new();
                }
                StatusCode::RANGE_NOT_SATISFIABLE => {
                    remove_if_present(request.part)?;
                    return Err(DownloadError::Status(status.as_u16()));
                }
                other => return Err(DownloadError::Status(other.as_u16())),
            }
        } else if !status.is_success() {
            return Err(DownloadError::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|announced| length + announced > request.size)
        {
            remove_if_present(request.part)?;
            return Err(DownloadError::SizeExceeded { size: request.size });
        }
        let received = self
            .stream(response, request, length, &mut hasher, cancel, progress)
            .await?;
        let total = length + received;
        if total < request.size {
            return Err(DownloadError::Incomplete {
                got: total,
                size: request.size,
            });
        }
        if hex_lower(&hasher.finalize()) != request.sha256 {
            remove_if_present(request.part)?;
            return Err(DownloadError::HashMismatch);
        }
        Ok(Received {
            resumed_from: length,
            bytes: received,
        })
    }

    /// Sends the request, following redirects by hand so that every hop is checked against the listed origins.
    async fn open(
        &self,
        request: &Request<'_>,
        offset: u64,
    ) -> Result<reqwest::Response, DownloadError> {
        let mut url = request.url.clone();
        for _ in 0..=MAX_REDIRECTS {
            let mut builder = self.http.get(url.clone());
            if offset > 0 {
                builder = builder.header(RANGE, format!("bytes={offset}-"));
            }
            let response = builder.send().await.map_err(network)?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let status = response.status().as_u16();
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .ok_or(DownloadError::Status(status))?;
            if !on_listed_origin(&next, request.origins) {
                return Err(DownloadError::OffOrigin {
                    host: host_of(&next),
                });
            }
            url = next;
        }
        Err(DownloadError::TooManyRedirects)
    }

    /// Appends the body to the .part file, stopping at the first byte past the stated size.
    async fn stream(
        &self,
        mut response: reqwest::Response,
        request: &Request<'_>,
        offset: u64,
        hasher: &mut Sha256,
        cancel: &Cancel,
        progress: &mut Progress<'_>,
    ) -> Result<u64, DownloadError> {
        if let Some(parent) = request.part.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(request.part)?;
        let mut received: u64 = 0;
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
            if offset + received + chunk.len() as u64 > request.size {
                drop(file);
                remove_if_present(request.part)?;
                return Err(DownloadError::SizeExceeded { size: request.size });
            }
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            received += chunk.len() as u64;
            progress(offset + received);
        }
        file.flush()?;
        Ok(received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_match_on_scheme_host_and_port() {
        let origins = [
            Url::parse("https://files.example.org").unwrap(),
            Url::parse("http://127.0.0.1:4321").unwrap(),
        ];
        assert!(on_listed_origin(
            &Url::parse("https://files.example.org/a/b.dll?x=1").unwrap(),
            &origins
        ));
        assert!(on_listed_origin(
            &Url::parse("http://127.0.0.1:4321/files/x").unwrap(),
            &origins
        ));
        assert!(!on_listed_origin(
            &Url::parse("http://files.example.org/a").unwrap(),
            &origins
        ));
        assert!(!on_listed_origin(
            &Url::parse("https://files.example.org:8443/a").unwrap(),
            &origins
        ));
        assert!(!on_listed_origin(
            &Url::parse("http://127.0.0.1:4322/files/x").unwrap(),
            &origins
        ));
    }

    #[test]
    fn files_hash_like_the_reference_vectors() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        fs::write(&empty, b"").unwrap();
        assert_eq!(
            sha256_of_file(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = dir.path().join("abc");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            sha256_of_file(&abc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_part_longer_than_the_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("x.part");
        fs::write(&part, b"too long").unwrap();
        let opened = Part::open(&part, 3).unwrap();
        assert_eq!(opened.length, 0);
        assert!(!part.exists());
        fs::write(&part, b"ab").unwrap();
        let opened = Part::open(&part, 3).unwrap();
        assert_eq!(opened.length, 2);
        assert!(part.exists());
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny HTTP server for tests: canned responses per path, one thread, no dependencies.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use url::Url;

struct Canned {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    sized: bool,
}

type Routes = Arc<Mutex<HashMap<String, Canned>>>;

pub struct CannedServer {
    addr: SocketAddr,
    origin: Url,
    routes: Routes,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CannedServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::spawn({
            let routes = routes.clone();
            let hits = hits.clone();
            let stop = stop.clone();
            move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        handle(stream, &routes, &hits);
                    }
                }
            }
        });
        Self {
            addr,
            origin: Url::parse(&format!("http://{addr}")).unwrap(),
            routes,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    pub fn origin(&self) -> &Url {
        &self.origin
    }

    pub fn json(&self, path: &str, status: u16, text: &str) {
        self.set(
            path,
            status,
            vec![("Content-Type".into(), "application/json".into())],
            text.as_bytes().to_vec(),
            true,
        );
    }

    pub fn bytes(&self, path: &str, body: Vec<u8>) {
        self.set(path, 200, Vec::new(), body, true);
    }

    /// Served without a Content-Length, so the client only learns the size by reading.
    pub fn bytes_unsized(&self, path: &str, body: Vec<u8>) {
        self.set(path, 200, Vec::new(), body, false);
    }

    pub fn redirect(&self, path: &str, location: &str) {
        self.set(
            path,
            302,
            vec![("Location".into(), location.into())],
            Vec::new(),
            true,
        );
    }

    pub fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }

    fn set(
        &self,
        path: &str,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        sized: bool,
    ) {
        self.routes.lock().unwrap().insert(
            path.to_string(),
            Canned {
                status,
                headers,
                body,
                sized,
            },
        );
    }
}

impl Drop for CannedServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(mut stream: TcpStream, routes: &Routes, hits: &Arc<Mutex<Vec<String>>>) {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    hits.lock().unwrap().push(path.clone());
    let routes = routes.lock().unwrap();
    let (status, headers, body, sized) = match routes.get(&path) {
        Some(canned) => (
            canned.status,
            canned.headers.clone(),
            canned.body.clone(),
            canned.sized,
        ),
        None => (404, Vec::new(), b"not found".to_vec(), true),
    };
    let mut head = format!("HTTP/1.1 {status} Canned\r\nConnection: close\r\n");
    if sized {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// A /api/v1/server body in the shape the backend serves, with the name and API version under test.
pub fn server_json(name: &str, api_version: u32) -> String {
    format!(
        r#"{{"name":"{name}","description":"A test server","website":"https://www.example.org","discordInvite":"https://discord.example.org/x","protocolId":"sug1_","gameRuntime":"skyrimse-1.7.104","apiVersion":{api_version},"manifestUrl":"/api/v1/manifest"}}"#
    )
}

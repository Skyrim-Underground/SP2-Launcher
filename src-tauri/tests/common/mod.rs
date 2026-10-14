// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts one of the Node servers under test/ and finds where it listens.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use url::Url;

pub struct NodeServer {
    child: Child,
    pub origin: Url,
}

impl NodeServer {
    /// `script` is a file name in test/; the server picks a free port and prints it on its first line.
    pub fn start(script: &str, args: &[&str]) -> Self {
        let path = format!("{}/../test/{script}", env!("CARGO_MANIFEST_DIR"));
        let mut child = Command::new("node")
            .arg(&path)
            .args(["--port", "0"])
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("node must be on PATH to run the test servers");
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let origin = line
            .trim()
            .strip_prefix("listening ")
            .unwrap_or_else(|| panic!("unexpected first line from {script}: {line:?}"));
        Self {
            child,
            origin: Url::parse(origin).unwrap(),
        }
    }
}

impl Drop for NodeServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

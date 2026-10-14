// SPDX-License-Identifier: AGPL-3.0-or-later
//! A plain-text log file with size rotation; every line passes through redaction first.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE_NAME: &str = "launcher.log";
pub const MAX_BYTES: u64 = 1024 * 1024;
/// The live file plus the numbered copies kept after rotation.
pub const KEEP: u32 = 5;

const TOKEN_PREFIXES: [&str; 2] = ["sugs_", "sugp_"];
const SCHEMES: [&str; 2] = ["https://", "http://"];
const AUTHORITY_END: &str = "/?#\"'<>)";
const URL_END: &str = "\"'<>)";

pub struct Logger {
    dir: PathBuf,
    writer: Mutex<()>,
}

impl Logger {
    pub fn open(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            writer: Mutex::new(()),
        })
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    pub fn info(&self, message: &str) {
        self.write("INFO", message);
    }

    pub fn warn(&self, message: &str) {
        self.write("WARN", message);
    }

    fn write(&self, level: &str, message: &str) {
        let line = format!("{} {level} {}\n", timestamp(), redact(message));
        let _serialized = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.path();
        let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        // A log that cannot be written must not stop the launcher, so failures here are dropped.
        if size >= MAX_BYTES && rotate(&self.dir).is_err() {
            return;
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// The oldest copy goes, every other copy moves up one number and the live file becomes the first copy.
fn rotate(dir: &Path) -> io::Result<()> {
    let oldest = numbered(dir, KEEP - 1);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for n in (1..KEEP - 1).rev() {
        let from = numbered(dir, n);
        if from.exists() {
            fs::rename(&from, numbered(dir, n + 1))?;
        }
    }
    fs::rename(dir.join(FILE_NAME), numbered(dir, 1))
}

fn numbered(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{FILE_NAME}.{n}"))
}

/// Replaces every session or play token with its prefix and reduces every URL to its scheme and host.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(prefix) = TOKEN_PREFIXES
            .iter()
            .find(|prefix| rest.starts_with(*prefix))
        {
            let token_len = rest[prefix.len()..]
                .bytes()
                .take_while(|byte| is_token_byte(*byte))
                .count();
            out.push_str(prefix);
            out.push_str("[redacted]");
            rest = &rest[prefix.len() + token_len..];
        } else if let Some(scheme) = SCHEMES.iter().find(|scheme| {
            rest.get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        }) {
            let after = &rest[scheme.len()..];
            let authority_len = after
                .find(|c: char| c.is_whitespace() || AUTHORITY_END.contains(c))
                .unwrap_or(after.len());
            let authority = &after[..authority_len];
            let host = authority.rsplit('@').next().unwrap_or(authority);
            let url_len = after
                .find(|c: char| c.is_whitespace() || URL_END.contains(c))
                .unwrap_or(after.len());
            out.push_str(&rest[..scheme.len()]);
            out.extend(host.chars().map(plain));
            rest = &after[url_len..];
        } else if let Some(ch) = rest.chars().next() {
            out.push(plain(ch));
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// A control character becomes a space, so no text that reaches the log can end its line or hide inside it.
fn plain(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_utc(seconds)
}

pub fn format_utc(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let second_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date, by the era arithmetic of Howard Hinnant.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "sugs_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCDEFG";
    const TOKEN: &str = "sugp_ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210_-abcdefg";
    const FULL_URL: &str = "https://backend.test:8443/api/v1/play?token=sugp_leak#frag";

    #[test]
    fn tokens_keep_only_their_prefix() {
        assert_eq!(
            redact(&format!("session {SESSION} stored")),
            "session sugs_[redacted] stored"
        );
        assert_eq!(redact(&format!("token={TOKEN}")), "token=sugp_[redacted]");
        assert_eq!(redact("sugs_"), "sugs_[redacted]");
    }

    #[test]
    fn urls_keep_only_scheme_and_host() {
        assert_eq!(
            redact(&format!("GET {FULL_URL} -> 200")),
            "GET https://backend.test:8443 -> 200"
        );
        assert_eq!(
            redact("fetch https://user:secret@files.test/a/b.7z"),
            "fetch https://files.test"
        );
        assert_eq!(redact("(see HTTP://X.test/p) ok"), "(see HTTP://X.test) ok");
        assert_eq!(redact("https://"), "https://");
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(
            redact("Skyrim läuft, 3/64 players"),
            "Skyrim läuft, 3/64 players"
        );
        assert_eq!(redact(""), "");
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(redact("a\nb"), "a b");
        assert_eq!(
            redact("refused (x\r\n1970-01-01T00:00:00Z INFO forged)"),
            "refused (x  1970-01-01T00:00:00Z INFO forged)"
        );
        assert_eq!(redact("tab\there\u{7f}"), "tab here ");
        assert_eq!(redact("https://h.test\u{1}/p"), "https://h.test ");
    }

    #[test]
    fn a_message_with_a_line_break_stays_one_line() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::open(root.path()).unwrap();
        logger.warn("refused: x\n1970-01-01T00:00:00Z INFO forged");
        let text = fs::read_to_string(logger.path()).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.ends_with(" WARN refused: x 1970-01-01T00:00:00Z INFO forged\n"));
    }

    #[test]
    fn timestamps_are_utc_iso() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(4_102_444_800), "2100-01-01T00:00:00Z");
        assert_eq!(format_utc(1_760_000_000), "2025-10-09T08:53:20Z");
    }

    #[test]
    fn the_log_file_leaks_no_session_token_or_full_url_but_keeps_the_host() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::open(root.path()).unwrap();
        logger.info(&format!("signed in, session {SESSION}"));
        logger.warn(&format!("play token {TOKEN} written"));
        logger.warn(&format!("GET {FULL_URL} failed"));
        let text = fs::read_to_string(logger.path()).unwrap();
        assert!(!text.contains(SESSION));
        assert!(!text.contains(TOKEN));
        assert!(!text.contains(FULL_URL));
        assert!(!text.contains("/api/v1/play"));
        assert!(text.contains("https://backend.test:8443"));
        assert_eq!(text.lines().count(), 3);
        for line in text.lines() {
            assert!(line.contains("Z INFO ") || line.contains("Z WARN "));
        }
    }

    #[test]
    fn rotation_keeps_five_files_of_about_a_mebibyte() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::open(root.path()).unwrap();
        let block = "x".repeat(64 * 1024);
        for _ in 0..(16 * 7) {
            logger.info(&block);
        }
        let mut names: Vec<String> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "launcher.log",
                "launcher.log.1",
                "launcher.log.2",
                "launcher.log.3",
                "launcher.log.4"
            ]
        );
        for name in &names {
            let size = fs::metadata(root.path().join(name)).unwrap().len();
            assert!(size <= MAX_BYTES + 64 * 1024 + 64, "{name} is {size} bytes");
        }
    }
}

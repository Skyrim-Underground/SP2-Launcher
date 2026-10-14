// SPDX-License-Identifier: AGPL-3.0-or-later
//! The server's manifest: its shape, the rules every manifest must pass, and the paths it may name.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::IgnoredAny;
use url::Url;

use crate::config::schema_version_of;
use crate::deploy::INSTALLED_FILE;
use crate::download::PART_SUFFIX;
use crate::servers::{OriginError, validate_origin};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_FILES: usize = 20_000;
/// Plugins are named relative to the game's Data folder.
const DATA_PREFIX: &str = "data/";
const NATIVE_EXTENSIONS: [&str; 3] = ["exe", "dll", "asi"];
const RESERVED_NAMES: [&str; 4] = ["con", "prn", "aux", "nul"];
const RESERVED_PREFIXES: [&str; 2] = ["com", "lpt"];
const RESERVED_DIGITS: &str = "0123456789\u{b9}\u{b2}\u{b3}";
const INVALID_CHARS: [char; 6] = ['<', '>', '"', '|', '?', '*'];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "$comment", default)]
    _comment: IgnoredAny,
    pub schema_version: u32,
    pub version: String,
    pub game_runtime: String,
    pub protocol_id: String,
    pub runtime: Runtime,
    pub launch: Launch,
    pub origins: Vec<Url>,
    pub files: Vec<FileEntry>,
    pub plugins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Runtime {
    pub exe: String,
    pub files: Vec<RuntimeFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Launch {
    pub loader: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileEntry {
    pub to: String,
    pub size: u64,
    pub sha256: String,
    pub native: bool,
    pub source: Source,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Source {
    Url {
        url: Url,
    },
    Archive {
        page: Url,
        archive_sha256: String,
        archive_size: u64,
        entry: String,
    },
}

impl Source {
    /// The address the file is fetched from, or the page the player fetches its archive from.
    pub fn location(&self) -> &Url {
        match self {
            Source::Url { url } => url,
            Source::Archive { page, .. } => page,
        }
    }

    /// The origin the file comes from, as shown to the player.
    pub fn origin(&self) -> String {
        self.location().origin().ascii_serialization()
    }
}

/// What a manifest must agree with: the runtime its server announced and the files the Steam Data folder holds.
pub struct Context<'a> {
    pub game_runtime: &'a str,
    /// Lower-case file names of the Steam copy's Data folder.
    pub steam_data: &'a HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("it is empty")]
    Empty,
    #[error("it is absolute")]
    Absolute,
    #[error("it contains a backslash")]
    Backslash,
    #[error("it contains a colon")]
    Colon,
    #[error("it contains a control character")]
    Control,
    #[error("it contains {0:?}")]
    Character(char),
    #[error("it has an empty segment")]
    EmptySegment,
    #[error("it has a \".\" or \"..\" segment")]
    DotSegment,
    #[error("a segment ends with a dot or a space")]
    TrailingDotOrSpace,
    #[error("it uses a name Windows reserves for devices")]
    Reserved,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("the manifest is larger than {MAX_BYTES} bytes")]
    TooLarge,
    #[error("the manifest is not valid: {0}")]
    Json(String),
    #[error("the manifest has schema version {0}, this launcher knows {SCHEMA_VERSION}")]
    Schema(u32),
    #[error("the manifest lists {0} files, more than the {MAX_FILES} this launcher allows")]
    TooManyFiles(usize),
    #[error("the manifest is for the game runtime {manifest}, but the server announces {server}")]
    Runtime { manifest: String, server: String },
    #[error("the path {path:?} is not allowed: {reason}")]
    Path { path: String, reason: PathError },
    #[error("{path:?} is listed twice")]
    Duplicate { path: String },
    #[error("the sha256 of {path:?} is not 64 lower-case hex characters")]
    Hash { path: String },
    #[error("{path:?} is an executable or a library but is not marked native")]
    NativeNotMarked { path: String },
    #[error("{path:?} is marked native but is not an exe, dll or asi file")]
    NativeMismarked { path: String },
    #[error("the loader {path:?} is not an exe file")]
    LoaderNotExe { path: String },
    #[error(
        "the loader {path:?} is neither the runtime exe nor a native file the manifest provides"
    )]
    LoaderUnknown { path: String },
    #[error("{path:?} is the name of the launcher's own install record")]
    InstallRecord { path: String },
    #[error("{path:?} ends with {PART_SUFFIX}, which the launcher keeps for unfinished downloads")]
    PartName { path: String },
    #[error("the origin {origin:?} is not usable: {reason}")]
    Origin { origin: String, reason: OriginError },
    #[error("the source of {path:?} is not on a listed origin")]
    OffOrigin { path: String },
    #[error("the plugin {name:?} must be a file name without a folder")]
    PluginName { name: String },
    #[error("the plugin {name:?} is not a file the manifest or the game provides")]
    UnknownPlugin { name: String },
}

/// Accepts a relative, /-separated path that stays inside the game folder on Windows.
pub fn check_path(path: &str) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.starts_with('/') {
        return Err(PathError::Absolute);
    }
    for c in path.chars() {
        match c {
            '\\' => return Err(PathError::Backslash),
            ':' => return Err(PathError::Colon),
            c if c.is_control() => return Err(PathError::Control),
            c if INVALID_CHARS.contains(&c) => return Err(PathError::Character(c)),
            _ => {}
        }
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err(PathError::EmptySegment);
        }
        if segment == "." || segment == ".." {
            return Err(PathError::DotSegment);
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(PathError::TrailingDotOrSpace);
        }
        if is_reserved(segment) {
            return Err(PathError::Reserved);
        }
    }
    Ok(())
}

/// Windows reserves device names whatever the extension, so only the part before the first dot counts.
pub(crate) fn is_reserved(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_lowercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return true;
    }
    RESERVED_PREFIXES.iter().any(|prefix| {
        stem.strip_prefix(prefix)
            .is_some_and(|rest| rest.chars().count() == 1 && RESERVED_DIGITS.contains(rest))
    })
}

/// The checked path as a path below the game folder.
pub fn relative_path(path: &str) -> Result<PathBuf, PathError> {
    check_path(path)?;
    Ok(path.split('/').collect())
}

pub fn is_native_name(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            NATIVE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn path_error(path: &str, reason: PathError) -> ManifestError {
    ManifestError::Path {
        path: path.to_string(),
        reason,
    }
}

fn hash_error(path: &str) -> ManifestError {
    ManifestError::Hash {
        path: path.to_string(),
    }
}

/// Size, shape and every rule, so a manifest that passes can be installed as it is.
pub fn parse(bytes: &[u8], context: &Context<'_>) -> Result<Manifest, ManifestError> {
    if bytes.len() > MAX_BYTES {
        return Err(ManifestError::TooLarge);
    }
    if let Some(version) = schema_version_of(bytes)
        && version != SCHEMA_VERSION
    {
        return Err(ManifestError::Schema(version));
    }
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|error| ManifestError::Json(error.to_string()))?;
    manifest.validate(context)?;
    Ok(manifest)
}

impl Manifest {
    pub fn validate(&self, context: &Context<'_>) -> Result<(), ManifestError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::Schema(self.schema_version));
        }
        if self.files.len() > MAX_FILES {
            return Err(ManifestError::TooManyFiles(self.files.len()));
        }
        if self.game_runtime != context.game_runtime {
            return Err(ManifestError::Runtime {
                manifest: self.game_runtime.clone(),
                server: context.game_runtime.to_string(),
            });
        }
        for origin in &self.origins {
            validate_origin(origin.as_str()).map_err(|reason| ManifestError::Origin {
                origin: origin.to_string(),
                reason,
            })?;
        }
        for path in [&self.runtime.exe, &self.launch.loader] {
            check_path(path).map_err(|reason| path_error(path, reason))?;
        }
        // Windows paths are case-insensitive, so the same file under two spellings is still a duplicate.
        let mut named: HashSet<String> = HashSet::new();
        for file in &self.runtime.files {
            check_path(&file.path).map_err(|reason| path_error(&file.path, reason))?;
            if !is_sha256(&file.sha256) {
                return Err(hash_error(&file.path));
            }
            if !named.insert(file.path.to_lowercase()) {
                return Err(ManifestError::Duplicate {
                    path: file.path.clone(),
                });
            }
        }
        for file in &self.files {
            self.check_file(file)?;
            if !named.insert(file.to.to_lowercase()) {
                return Err(ManifestError::Duplicate {
                    path: file.to.clone(),
                });
            }
        }
        self.check_loader()?;
        for name in &self.plugins {
            check_path(name).map_err(|reason| path_error(name, reason))?;
            if name.contains('/') {
                return Err(ManifestError::PluginName { name: name.clone() });
            }
            let lower = name.to_lowercase();
            if !named.contains(&format!("{DATA_PREFIX}{lower}"))
                && !context.steam_data.contains(&lower)
            {
                return Err(ManifestError::UnknownPlugin { name: name.clone() });
            }
        }
        Ok(())
    }

    fn check_file(&self, file: &FileEntry) -> Result<(), ManifestError> {
        check_path(&file.to).map_err(|reason| path_error(&file.to, reason))?;
        if !is_sha256(&file.sha256) {
            return Err(hash_error(&file.to));
        }
        match (is_native_name(&file.to), file.native) {
            (true, false) => {
                return Err(ManifestError::NativeNotMarked {
                    path: file.to.clone(),
                });
            }
            (false, true) => {
                return Err(ManifestError::NativeMismarked {
                    path: file.to.clone(),
                });
            }
            _ => {}
        }
        // The launcher's record and its unfinished downloads live in the game folder too, so a manifest may not name them.
        let name = file
            .to
            .rsplit('/')
            .next()
            .unwrap_or(&file.to)
            .to_ascii_lowercase();
        if name == INSTALLED_FILE {
            return Err(ManifestError::InstallRecord {
                path: file.to.clone(),
            });
        }
        if name.ends_with(PART_SUFFIX) {
            return Err(ManifestError::PartName {
                path: file.to.clone(),
            });
        }
        if !self.on_listed_origin(file.source.location()) {
            return Err(ManifestError::OffOrigin {
                path: file.to.clone(),
            });
        }
        if let Source::Archive {
            archive_sha256,
            entry,
            ..
        } = &file.source
        {
            if !is_sha256(archive_sha256) {
                return Err(hash_error(&file.to));
            }
            check_path(entry).map_err(|reason| path_error(entry, reason))?;
        }
        Ok(())
    }

    /// The loader is run as it is, so it must be the game's own exe or an exe the manifest ships and shows as native.
    fn check_loader(&self) -> Result<(), ManifestError> {
        let loader = self.launch.loader.to_lowercase();
        if !loader.ends_with(".exe") {
            return Err(ManifestError::LoaderNotExe {
                path: self.launch.loader.clone(),
            });
        }
        let provided = loader == self.runtime.exe.to_lowercase()
            || self
                .files
                .iter()
                .any(|file| file.native && file.to.to_lowercase() == loader);
        if provided {
            Ok(())
        } else {
            Err(ManifestError::LoaderUnknown {
                path: self.launch.loader.clone(),
            })
        }
    }

    pub fn on_listed_origin(&self, url: &Url) -> bool {
        self.origins
            .iter()
            .any(|origin| origin.origin() == url.origin())
    }

    /// The files that run as native code, with where each comes from.
    pub fn native_files(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.iter().filter(|file| file.native)
    }

    /// Every path the manifest places, lower-cased because Windows paths are case-insensitive.
    pub fn named_paths(&self) -> HashSet<String> {
        self.files
            .iter()
            .map(|file| file.to.to_lowercase())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const FIXTURE: &str = include_str!("../../test/fixtures/manifest.json");
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn steam_data() -> HashSet<String> {
        ["skyrim.esm", "update.esm", "dawnguard.esm"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn parse_with(text: &str, data: &HashSet<String>) -> Result<Manifest, ManifestError> {
        parse(
            text.as_bytes(),
            &Context {
                game_runtime: "skyrimse-1.7.104",
                steam_data: data,
            },
        )
    }

    /// The fixture with one file entry replaced, so each rule is tested on an otherwise valid manifest.
    /// The loader becomes the runtime exe, since the file that provided it is gone.
    fn with_file(entry: &str) -> String {
        let start = FIXTURE.find("\n  \"files\": [").unwrap();
        let end = FIXTURE[start..].find("\n  ],").unwrap() + start;
        format!(
            "{}\n  \"files\": [\n    {entry}{}",
            &FIXTURE[..start],
            &FIXTURE[end..]
        )
        .replace(", \"Example.esp\"", "")
        .replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"SkyrimSE.exe\"",
        )
    }

    fn url_file(to: &str, native: bool) -> String {
        format!(
            r#"{{ "to": "{to}", "size": 1, "sha256": "{HASH}", "native": {native}, "source": {{ "kind": "url", "url": "https://files.example.org/x" }} }}"#
        )
    }

    #[test]
    fn the_fixture_passes_every_rule() {
        let manifest = parse_with(FIXTURE, &steam_data()).unwrap();
        assert_eq!(manifest.version, "2026.10.13-1");
        assert_eq!(manifest.files.len(), 3);
        assert_eq!(manifest.native_files().count(), 2);
        assert_eq!(
            manifest.files[0].source.origin(),
            "https://files.example.org"
        );
        assert_eq!(manifest.plugins.len(), 4);
    }

    #[test]
    fn traversal_absolute_and_reserved_paths_are_refused() {
        let refused = [
            ("../SkyrimSE.exe", PathError::DotSegment),
            ("Data/../x.esp", PathError::DotSegment),
            ("./x.esp", PathError::DotSegment),
            ("/x.esp", PathError::Absolute),
            ("C:/x.esp", PathError::Colon),
            ("Data:x.esp", PathError::Colon),
            ("Data\\x.esp", PathError::Backslash),
            ("\\\\server\\share\\x.esp", PathError::Backslash),
            ("Data//x.esp", PathError::EmptySegment),
            ("Data/x.esp/", PathError::EmptySegment),
            ("", PathError::Empty),
            ("Data/x\u{7}.esp", PathError::Control),
            ("Data/x\n.esp", PathError::Control),
            ("Data/x*.esp", PathError::Character('*')),
            ("Data/x.esp.", PathError::TrailingDotOrSpace),
            ("Data /x.esp", PathError::TrailingDotOrSpace),
            ("CON", PathError::Reserved),
            ("Data/nul.esp", PathError::Reserved),
            ("Data/COM1.txt", PathError::Reserved),
            ("LPT9", PathError::Reserved),
            ("Data/lpt\u{b2}", PathError::Reserved),
            ("Data/aux .esp", PathError::Reserved),
        ];
        for (path, reason) in refused {
            assert_eq!(check_path(path), Err(reason), "{path:?}");
        }
        for path in [
            "SkyrimSE.exe",
            "Data/Skyrim.esm",
            "Data/SKSE/Plugins/x.dll",
            "Data/com.esp",
            "Data/console.txt",
            "Data/lpt10.txt",
            "Data/.hidden",
        ] {
            assert_eq!(check_path(path), Ok(()), "{path:?}");
        }
        assert_eq!(
            relative_path("Data/SKSE/Plugins/x.dll").unwrap(),
            PathBuf::from("Data")
                .join("SKSE")
                .join("Plugins")
                .join("x.dll")
        );
        assert!(matches!(
            parse_with(&with_file(&url_file("../x.esp", false)), &steam_data()),
            Err(ManifestError::Path {
                reason: PathError::DotSegment,
                ..
            })
        ));
        assert!(matches!(
            parse_with(&with_file(&url_file("Data/NUL", false)), &steam_data()),
            Err(ManifestError::Path {
                reason: PathError::Reserved,
                ..
            })
        ));
    }

    #[test]
    fn the_native_flag_must_match_the_extension() {
        assert!(is_native_name("skse64_loader.exe"));
        assert!(is_native_name("Data/SKSE/Plugins/X.DLL"));
        assert!(is_native_name("Data/x.asi"));
        assert!(!is_native_name("Data/x.esp"));
        assert!(!is_native_name("Data/exe"));
        assert!(!is_native_name("Data/x.dll.txt"));
        assert!(matches!(
            parse_with(&with_file(&url_file("Data/x.dll", false)), &steam_data()),
            Err(ManifestError::NativeNotMarked { path }) if path == "Data/x.dll"
        ));
        assert!(matches!(
            parse_with(&with_file(&url_file("Data/X.ASI", false)), &steam_data()),
            Err(ManifestError::NativeNotMarked { .. })
        ));
        assert!(matches!(
            parse_with(&with_file(&url_file("Data/x.esp", true)), &steam_data()),
            Err(ManifestError::NativeMismarked { path }) if path == "Data/x.esp"
        ));
        assert!(parse_with(&with_file(&url_file("Data/x.dll", true)), &steam_data()).is_ok());
    }

    #[test]
    fn the_launchers_own_file_names_are_refused() {
        for path in ["installed.json", "Data/INSTALLED.JSON"] {
            assert!(
                matches!(
                    parse_with(&with_file(&url_file(path, false)), &steam_data()),
                    Err(ManifestError::InstallRecord { path: refused }) if refused == path
                ),
                "{path:?}"
            );
        }
        for path in ["Data/x.esp.part", "Data/X.PART", "x.part"] {
            assert!(
                matches!(
                    parse_with(&with_file(&url_file(path, false)), &steam_data()),
                    Err(ManifestError::PartName { path: refused }) if refused == path
                ),
                "{path:?}"
            );
        }
        for path in ["Data/installed.json.txt", "Data/part", "Data/parts/x.esp"] {
            assert!(
                parse_with(&with_file(&url_file(path, false)), &steam_data()).is_ok(),
                "{path:?}"
            );
        }
    }

    #[test]
    fn hashes_are_lower_case_hex_of_sixty_four() {
        let upper = url_file("Data/x.esp", false).replace(HASH, &HASH.to_uppercase());
        assert!(matches!(
            parse_with(&with_file(&upper), &steam_data()),
            Err(ManifestError::Hash { .. })
        ));
        let short = url_file("Data/x.esp", false).replace(HASH, &HASH[1..]);
        assert!(matches!(
            parse_with(&with_file(&short), &steam_data()),
            Err(ManifestError::Hash { .. })
        ));
        let missing_size = url_file("Data/x.esp", false).replace(r#""size": 1, "#, "");
        assert!(matches!(
            parse_with(&with_file(&missing_size), &steam_data()),
            Err(ManifestError::Json(_))
        ));
    }

    #[test]
    fn every_source_must_sit_on_a_listed_origin() {
        let elsewhere = url_file("Data/x.esp", false)
            .replace("https://files.example.org/x", "https://other.example.org/x");
        assert!(matches!(
            parse_with(&with_file(&elsewhere), &steam_data()),
            Err(ManifestError::OffOrigin { path }) if path == "Data/x.esp"
        ));
        let other_port = url_file("Data/x.esp", false).replace(
            "https://files.example.org/x",
            "https://files.example.org:8443/x",
        );
        assert!(matches!(
            parse_with(&with_file(&other_port), &steam_data()),
            Err(ManifestError::OffOrigin { .. })
        ));
        let page = format!(
            r#"{{ "to": "Data/x.esp", "size": 1, "sha256": "{HASH}", "native": false, "source": {{ "kind": "archive", "page": "https://downloads.example.org/mods/1", "archiveSha256": "{HASH}", "archiveSize": 2, "entry": "x.esp" }} }}"#
        );
        assert!(matches!(
            parse_with(&with_file(&page), &steam_data()),
            Err(ManifestError::OffOrigin { .. })
        ));
        let http_origin = FIXTURE.replace(
            "\"https://files.example.org\"",
            "\"http://files.example.org\"",
        );
        assert!(matches!(
            parse_with(&http_origin, &steam_data()),
            Err(ManifestError::Origin {
                reason: OriginError::NotHttps,
                ..
            })
        ));
        let with_path = FIXTURE.replace(
            "\"https://files.example.org\"",
            "\"https://files.example.org/mods\"",
        );
        assert!(matches!(
            parse_with(&with_path, &steam_data()),
            Err(ManifestError::Origin {
                reason: OriginError::Path,
                ..
            })
        ));
    }

    #[test]
    fn plugins_come_from_the_runtime_the_files_or_the_steam_data_folder() {
        let unknown = FIXTURE.replace("\"Example.esp\"", "\"Missing.esp\"");
        assert!(matches!(
            parse_with(&unknown, &steam_data()),
            Err(ManifestError::UnknownPlugin { name }) if name == "Missing.esp"
        ));
        let nested = FIXTURE.replace("\"Example.esp\"", "\"Data/Example.esp\"");
        assert!(matches!(
            parse_with(&nested, &steam_data()),
            Err(ManifestError::PluginName { .. })
        ));
        let traversal = FIXTURE.replace("\"Example.esp\"", "\"..\"");
        assert!(matches!(
            parse_with(&traversal, &steam_data()),
            Err(ManifestError::Path { .. })
        ));
        let no_steam_files = HashSet::new();
        assert!(matches!(
            parse_with(FIXTURE, &no_steam_files),
            Err(ManifestError::UnknownPlugin { name }) if name == "Update.esm"
        ));
        let spelled = FIXTURE.replace("\"Example.esp\"", "\"EXAMPLE.ESP\"");
        assert!(parse_with(&spelled, &steam_data()).is_ok());
    }

    #[test]
    fn the_loader_is_the_runtime_exe_or_a_native_exe_the_manifest_ships() {
        let runtime_exe = FIXTURE.replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"skyrimse.exe\"",
        );
        assert!(parse_with(&runtime_exe, &steam_data()).is_ok());
        let spelled = FIXTURE.replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"SKSE64_LOADER.EXE\"",
        );
        assert!(parse_with(&spelled, &steam_data()).is_ok());
        let dll = FIXTURE.replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"Data/SKSE/Plugins/Example.dll\"",
        );
        assert!(matches!(
            parse_with(&dll, &steam_data()),
            Err(ManifestError::LoaderNotExe { path }) if path == "Data/SKSE/Plugins/Example.dll"
        ));
        let script = FIXTURE.replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"start.cmd\"",
        );
        assert!(matches!(
            parse_with(&script, &steam_data()),
            Err(ManifestError::LoaderNotExe { .. })
        ));
        let unlisted = FIXTURE.replace(
            "\"loader\": \"skse64_loader.exe\"",
            "\"loader\": \"other.exe\"",
        );
        assert!(matches!(
            parse_with(&unlisted, &steam_data()),
            Err(ManifestError::LoaderUnknown { path }) if path == "other.exe"
        ));
    }

    #[test]
    fn the_runtime_must_be_the_one_the_server_announces() {
        let result = parse(
            FIXTURE.as_bytes(),
            &Context {
                game_runtime: "skyrimse-1.6.1170",
                steam_data: &steam_data(),
            },
        );
        assert!(matches!(
            result,
            Err(ManifestError::Runtime { manifest, server })
                if manifest == "skyrimse-1.7.104" && server == "skyrimse-1.6.1170"
        ));
    }

    #[test]
    fn shape_limits_and_duplicates_are_refused() {
        let newer = FIXTURE.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert!(matches!(
            parse_with(&newer, &steam_data()),
            Err(ManifestError::Schema(2))
        ));
        let extra = FIXTURE.replace("\"plugins\"", "\"telemetry\": true,\n  \"plugins\"");
        assert!(matches!(
            parse_with(&extra, &steam_data()),
            Err(ManifestError::Json(_))
        ));
        let unknown_kind = FIXTURE.replace("\"kind\": \"archive\"", "\"kind\": \"zip\"");
        assert!(matches!(
            parse_with(&unknown_kind, &steam_data()),
            Err(ManifestError::Json(_))
        ));
        let big = vec![b' '; MAX_BYTES + 1];
        assert!(matches!(
            parse(
                &big,
                &Context {
                    game_runtime: "x",
                    steam_data: &steam_data()
                }
            ),
            Err(ManifestError::TooLarge)
        ));
        let duplicate = with_file(&format!(
            "{},\n    {}",
            url_file("Data/x.esp", false),
            url_file("data/X.ESP", false)
        ));
        assert!(matches!(
            parse_with(&duplicate, &steam_data()),
            Err(ManifestError::Duplicate { path }) if path == "data/X.ESP"
        ));
        let clashes_with_runtime = with_file(&url_file("data/skyrim.esm", false));
        assert!(matches!(
            parse_with(&clashes_with_runtime, &steam_data()),
            Err(ManifestError::Duplicate { .. })
        ));
        let mut many = parse_with(FIXTURE, &steam_data()).unwrap();
        let template = many.files[2].clone();
        many.files = (0..=MAX_FILES)
            .map(|n| FileEntry {
                to: format!("Data/{n}.esp"),
                ..template.clone()
            })
            .collect();
        assert!(matches!(
            many.validate(&Context {
                game_runtime: "skyrimse-1.7.104",
                steam_data: &steam_data()
            }),
            Err(ManifestError::TooManyFiles(n)) if n == MAX_FILES + 1
        ));
    }
}

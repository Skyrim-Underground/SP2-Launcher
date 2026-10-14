// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player's Steam copy of the game: where it is, which build it is, and whether a game folder still matches a server's runtime.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::{HSTRING, PCWSTR};

use crate::manifest::{PathError, RuntimeFile, relative_path};

pub const APP_ID: &str = "489830";
pub const EXE: &str = "SkyrimSE.exe";
pub const DATA_DIR: &str = "Data";
/// The game keeps plugins.txt here under the user's local app data, whichever copy of the game runs.
pub const PROFILE_DIR: &str = "Skyrim Special Edition";
pub const PLUGINS_FILE: &str = "plugins.txt";
const DEFAULT_INSTALL_DIR: &str = "Skyrim Special Edition";
const STEAMAPPS: &str = "steamapps";
const LIBRARIES_FILE: &str = "libraryfolders.vdf";
const RUNTIME_PREFIX: &str = "skyrimse-";
/// The root block of a version resource, which holds the fixed file information.
const VERSION_ROOT: [u16; 2] = [b'\\' as u16, 0];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GameError {
    #[error("Steam is not installed for this Windows user")]
    NoSteam,
    #[error("Skyrim Special Edition is not installed in any Steam library")]
    NotInstalled,
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("{path} has no readable version information: {source}")]
    Unavailable {
        path: String,
        source: windows::core::Error,
    },
    #[error("{path} has version information without the fixed block")]
    NoFixedInfo { path: String },
}

/// Why a game folder cannot run a server's runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the Steam copy of the game is build {detected}, but the server needs {announced}")]
    Build { detected: String, announced: String },
    #[error("the runtime path {path:?} is not allowed: {reason}")]
    Path { path: String, reason: PathError },
    #[error("{path} is missing from the game folder")]
    Missing { path: String },
    #[error("{path} is {actual} bytes, the server's runtime has {expected}")]
    Size {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("{path} does not match the server's runtime")]
    Hash { path: String },
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
}

/// The four numbers of a PE file's FileVersion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

impl FileVersion {
    /// The runtime a server names for this build: the first three numbers behind the game's prefix.
    pub fn runtime_id(&self) -> String {
        format!(
            "{RUNTIME_PREFIX}{}.{}.{}",
            self.major, self.minor, self.build
        )
    }
}

impl fmt::Display for FileVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.major, self.minor, self.build, self.revision
        )
    }
}

/// The plugins.txt the game reads, under the user's local app data folder.
pub fn plugins_path(local_app_data: &Path) -> PathBuf {
    local_app_data.join(PROFILE_DIR).join(PLUGINS_FILE)
}

/// The FileVersion of a PE file's version resource.
pub fn file_version(path: &Path) -> Result<FileVersion, VersionError> {
    let text = path.to_string_lossy().into_owned();
    let name = HSTRING::from(path.as_os_str());
    // SAFETY: the name outlives the calls, the buffer is as long as the first call asked for, and the query's pointer is read while the buffer lives.
    unsafe {
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return Err(VersionError::Unavailable {
                path: text,
                source: windows::core::Error::from_thread(),
            });
        }
        let mut buffer = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, buffer.as_mut_ptr().cast()).map_err(|source| {
            VersionError::Unavailable {
                path: text.clone(),
                source,
            }
        })?;
        let mut info: *mut std::ffi::c_void = ptr::null_mut();
        let mut length: u32 = 0;
        let found = VerQueryValueW(
            buffer.as_ptr().cast(),
            PCWSTR::from_raw(VERSION_ROOT.as_ptr()),
            &mut info,
            &mut length,
        )
        .as_bool();
        if !found || info.is_null() || (length as usize) < mem::size_of::<VS_FIXEDFILEINFO>() {
            return Err(VersionError::NoFixedInfo { path: text });
        }
        let fixed: VS_FIXEDFILEINFO = ptr::read_unaligned(info.cast());
        Ok(FileVersion {
            major: (fixed.dwFileVersionMS >> 16) as u16,
            minor: fixed.dwFileVersionMS as u16,
            build: (fixed.dwFileVersionLS >> 16) as u16,
            revision: fixed.dwFileVersionLS as u16,
        })
    }
}

/// The runtime a server announces must be the build of the exe the player has.
pub fn check_build(detected: &FileVersion, announced: &str) -> Result<(), RuntimeError> {
    let detected = detected.runtime_id();
    if detected == announced {
        Ok(())
    } else {
        Err(RuntimeError::Build {
            detected,
            announced: announced.to_string(),
        })
    }
}

/// The hashed runtime files in a game folder: each present, of the stated size and with the stated hash.
/// `hash_of` is asked for the hash of a file by its manifest path, so a caller can answer from a cache.
pub fn check_runtime_files(
    game_folder: &Path,
    files: &[RuntimeFile],
    hash_of: &mut dyn FnMut(&str, &Path, &fs::Metadata) -> io::Result<String>,
) -> Result<(), RuntimeError> {
    for file in files {
        let relative = relative_path(&file.path).map_err(|reason| RuntimeError::Path {
            path: file.path.clone(),
            reason,
        })?;
        let full = game_folder.join(relative);
        let meta = match fs::metadata(&full) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => {
                return Err(RuntimeError::Missing {
                    path: file.path.clone(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(RuntimeError::Missing {
                    path: file.path.clone(),
                });
            }
            Err(error) => {
                return Err(RuntimeError::Io {
                    path: file.path.clone(),
                    source: error,
                });
            }
        };
        if meta.len() != file.size {
            return Err(RuntimeError::Size {
                path: file.path.clone(),
                expected: file.size,
                actual: meta.len(),
            });
        }
        let hash = hash_of(&file.path, &full, &meta).map_err(|source| RuntimeError::Io {
            path: file.path.clone(),
            source,
        })?;
        if hash != file.sha256 {
            return Err(RuntimeError::Hash {
                path: file.path.clone(),
            });
        }
    }
    Ok(())
}

/// The game folder of the Steam copy, found through Steam's own records.
pub fn find_skyrim() -> Result<PathBuf, GameError> {
    let steam = steam_root().ok_or(GameError::NoSteam)?;
    find_in_libraries(&steam)
}

/// Steam's own folder, from the user's registry key first and the machine-wide one second.
fn steam_root() -> Option<PathBuf> {
    registry_string(HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath")
        .or_else(|| {
            registry_string(
                HKEY_LOCAL_MACHINE,
                "SOFTWARE\\WOW6432Node\\Valve\\Steam",
                "InstallPath",
            )
        })
        .map(|text| PathBuf::from(text.replace('/', "\\")))
}

fn registry_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let subkey = HSTRING::from(subkey);
    let value = HSTRING::from(value);
    let mut length: u32 = 0;
    // SAFETY: the names outlive both calls and the buffer is as long as the first call asked for.
    unsafe {
        let sized = RegGetValueW(
            root,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut length),
        );
        if sized != ERROR_SUCCESS || length == 0 {
            return None;
        }
        let mut buffer = vec![0u16; (length as usize).div_ceil(2)];
        let read = RegGetValueW(
            root,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut length),
        );
        if read != ERROR_SUCCESS {
            return None;
        }
        let chars = (length as usize / 2).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..chars]);
        Some(text.trim_end_matches('\0').to_string())
    }
}

/// Every Steam library: the Steam folder itself, then the ones libraryfolders.vdf lists.
pub fn libraries(steam: &Path) -> Vec<PathBuf> {
    let mut found = vec![steam.to_path_buf()];
    let listed = fs::read_to_string(steam.join(STEAMAPPS).join(LIBRARIES_FILE))
        .ok()
        .map(|text| library_paths(&text))
        .unwrap_or_default();
    for library in listed {
        let path = PathBuf::from(library.replace('/', "\\"));
        if !found.iter().any(|known| same_path(known, &path)) {
            found.push(path);
        }
    }
    found
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}

/// The game folder in the first library that holds the game's app record, or that has its default folder.
pub fn find_in_libraries(steam: &Path) -> Result<PathBuf, GameError> {
    for library in libraries(steam) {
        let steamapps = library.join(STEAMAPPS);
        let record = steamapps.join(format!("appmanifest_{APP_ID}.acf"));
        let install_dir = fs::read_to_string(&record)
            .ok()
            .and_then(|text| app_install_dir(&text))
            .unwrap_or_else(|| DEFAULT_INSTALL_DIR.to_string());
        let game = steamapps.join("common").join(install_dir);
        if game.join(EXE).is_file() {
            return Ok(game);
        }
    }
    Err(GameError::NotInstalled)
}

/// Lower-case names of the files in the game's Data folder, for the manifest's plugin rule.
pub fn data_files(game: &Path) -> io::Result<HashSet<String>> {
    let mut names = HashSet::new();
    for entry in fs::read_dir(game.join(DATA_DIR))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.insert(entry.file_name().to_string_lossy().to_lowercase());
        }
    }
    Ok(names)
}

/// Steam's KeyValues text: quoted keys with a quoted value or a brace block.
#[derive(Debug, PartialEq, Eq)]
enum Kv {
    Text(String),
    Block(Vec<(String, Kv)>),
}

fn parse_keyvalues(text: &str) -> Vec<(String, Kv)> {
    let mut tokens = tokenize(text).into_iter().peekable();
    parse_block(&mut tokens)
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    Text(String),
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut value = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => value.push(chars.next().unwrap_or('\\')),
                        other => value.push(other),
                    }
                }
                tokens.push(Token::Text(value));
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            other => {
                let mut value = String::from(other);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || next == '{' || next == '}' || next == '"' {
                        break;
                    }
                    value.push(next);
                    chars.next();
                }
                tokens.push(Token::Text(value));
            }
        }
    }
    tokens
}

fn parse_block(tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>) -> Vec<(String, Kv)> {
    let mut pairs = Vec::new();
    while let Some(token) = tokens.next() {
        let key = match token {
            Token::Close => break,
            Token::Open => continue,
            Token::Text(key) => key,
        };
        match tokens.next() {
            Some(Token::Open) => pairs.push((key, Kv::Block(parse_block(tokens)))),
            Some(Token::Text(value)) => pairs.push((key, Kv::Text(value))),
            Some(Token::Close) | None => break,
        }
    }
    pairs
}

fn lookup<'a>(pairs: &'a [(String, Kv)], key: &str) -> Option<&'a Kv> {
    pairs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

/// Library paths in both layouts Steam has used: a block with a "path" key, or the path as the value itself.
fn library_paths(text: &str) -> Vec<String> {
    let root = parse_keyvalues(text);
    let Some(Kv::Block(entries)) = lookup(&root, "libraryfolders") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter(|(key, _)| key.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|(_, value)| match value {
            Kv::Text(path) => Some(path.clone()),
            Kv::Block(fields) => match lookup(fields, "path") {
                Some(Kv::Text(path)) => Some(path.clone()),
                _ => None,
            },
        })
        .collect()
}

fn app_install_dir(text: &str) -> Option<String> {
    let root = parse_keyvalues(text);
    let Kv::Block(state) = lookup(&root, "AppState")? else {
        return None;
    };
    match lookup(state, "installdir")? {
        Kv::Text(dir) => Some(dir.clone()),
        Kv::Block(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARIES: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"label"		""
		"apps"
		{
			"228980"		"132691472"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"apps"
		{
			"489830"		"16092533099"
		}
	}
	"contentstatsid"		"-1"
}
"#;

    #[test]
    fn keyvalues_text_parses_into_nested_pairs() {
        let parsed = parse_keyvalues(
            "// comment\n\"AppState\"\n{\n\t\"appid\"\t\t\"489830\"\n\t\"installdir\"\t\t\"Skyrim \\\"Special\\\" Edition\"\n\t\"UserConfig\"\n\t{\n\t}\n}\n",
        );
        let Some(Kv::Block(state)) = lookup(&parsed, "AppState") else {
            panic!("expected the AppState block, got {parsed:?}");
        };
        assert_eq!(
            lookup(state, "appid"),
            Some(&Kv::Text("489830".to_string()))
        );
        assert_eq!(
            app_install_dir(
                "\"AppState\"\n{\n\"installdir\"\t\"Skyrim \\\"Special\\\" Edition\"\n}"
            ),
            Some("Skyrim \"Special\" Edition".to_string())
        );
        assert_eq!(app_install_dir("not keyvalues at all"), None);
    }

    #[test]
    fn library_paths_come_in_both_layouts() {
        assert_eq!(
            library_paths(LIBRARIES),
            vec!["C:\\Program Files (x86)\\Steam", "D:\\SteamLibrary"]
        );
        let old = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\t\"1\"\n\t\"1\"\t\t\"E:\\\\Games\"\n}";
        assert_eq!(library_paths(old), vec!["E:\\Games"]);
        assert!(library_paths("").is_empty());
    }

    #[test]
    fn the_game_is_found_in_the_library_that_records_it() {
        let root = tempfile::tempdir().unwrap();
        let steam = root.path().join("Steam");
        let other = root.path().join("Library");
        fs::create_dir_all(steam.join(STEAMAPPS)).unwrap();
        let listed = format!(
            "\"libraryfolders\"\n{{\n\"0\"\n{{\n\"path\"\t\"{}\"\n}}\n\"1\"\n{{\n\"path\"\t\"{}\"\n}}\n}}",
            steam.to_string_lossy().replace('\\', "\\\\"),
            other.to_string_lossy().replace('\\', "\\\\")
        );
        fs::write(steam.join(STEAMAPPS).join(LIBRARIES_FILE), listed).unwrap();
        assert!(matches!(
            find_in_libraries(&steam),
            Err(GameError::NotInstalled)
        ));
        let game = other
            .join(STEAMAPPS)
            .join("common")
            .join("Skyrim SE renamed");
        fs::create_dir_all(game.join(DATA_DIR)).unwrap();
        fs::write(game.join(EXE), b"exe").unwrap();
        fs::write(game.join(DATA_DIR).join("Skyrim.esm"), b"esm").unwrap();
        fs::create_dir_all(game.join(DATA_DIR).join("Scripts")).unwrap();
        fs::write(
            other
                .join(STEAMAPPS)
                .join(format!("appmanifest_{APP_ID}.acf")),
            "\"AppState\"\n{\n\"installdir\"\t\"Skyrim SE renamed\"\n}",
        )
        .unwrap();
        assert_eq!(find_in_libraries(&steam).unwrap(), game);
        let names = data_files(&game).unwrap();
        assert!(names.contains("skyrim.esm"));
        assert!(!names.contains("scripts"));
        assert_eq!(libraries(&steam).len(), 2);
    }

    #[test]
    fn the_runtime_id_is_the_first_three_numbers_of_the_version() {
        let version = FileVersion {
            major: 1,
            minor: 7,
            build: 104,
            revision: 0,
        };
        assert_eq!(version.to_string(), "1.7.104.0");
        assert_eq!(version.runtime_id(), "skyrimse-1.7.104");
        assert!(check_build(&version, "skyrimse-1.7.104").is_ok());
        assert!(matches!(
            check_build(&version, "skyrimse-1.6.1170"),
            Err(RuntimeError::Build { detected, announced })
                if detected == "skyrimse-1.7.104" && announced == "skyrimse-1.6.1170"
        ));
        assert!(matches!(
            check_build(&version, "SKYRIMSE-1.7.104"),
            Err(RuntimeError::Build { .. })
        ));
        assert_eq!(
            plugins_path(Path::new("C:\\Users\\p\\AppData\\Local")),
            PathBuf::from("C:\\Users\\p\\AppData\\Local\\Skyrim Special Edition\\plugins.txt")
        );
    }

    #[test]
    fn the_file_version_is_read_from_a_system_library_and_refused_for_a_plain_file() {
        let system = std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows");
        let kernel = Path::new(&system).join("System32").join("kernel32.dll");
        let version = file_version(&kernel).unwrap();
        assert!(version.major >= 6, "{version}");
        assert!(version.runtime_id().starts_with("skyrimse-"));
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.exe");
        fs::write(&plain, b"not a PE file").unwrap();
        assert!(matches!(
            file_version(&plain),
            Err(VersionError::Unavailable { .. })
        ));
        assert!(matches!(
            file_version(&dir.path().join("missing.exe")),
            Err(VersionError::Unavailable { .. })
        ));
    }

    #[test]
    #[ignore = "needs the Steam copy of the game on this PC; run with --ignored"]
    fn the_steam_copy_on_this_pc_reports_its_build() {
        let steam = find_skyrim().expect("Steam and the game must be installed");
        let version = file_version(&steam.join(EXE)).unwrap();
        println!("{} is {version}, runtime {}", EXE, version.runtime_id());
        assert_eq!(version.major, 1);
    }

    #[test]
    fn runtime_files_are_checked_for_presence_size_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Data")).unwrap();
        fs::write(dir.path().join("Data").join("Skyrim.esm"), b"abc").unwrap();
        let mut asked = Vec::new();
        let mut hash_of = |key: &str, path: &Path, _: &fs::Metadata| {
            asked.push(key.to_string());
            crate::download::sha256_of_file(path)
        };
        let file = |size: u64, sha256: &str| RuntimeFile {
            path: "Data/Skyrim.esm".to_string(),
            size,
            sha256: sha256.to_string(),
        };
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(check_runtime_files(dir.path(), &[file(3, abc)], &mut hash_of).is_ok());
        assert!(matches!(
            check_runtime_files(dir.path(), &[file(4, abc)], &mut hash_of),
            Err(RuntimeError::Size {
                expected: 4,
                actual: 3,
                ..
            })
        ));
        assert!(matches!(
            check_runtime_files(dir.path(), &[file(3, &abc.replace('b', "c"))], &mut hash_of),
            Err(RuntimeError::Hash { path }) if path == "Data/Skyrim.esm"
        ));
        let missing = RuntimeFile {
            path: "SkyrimSE.exe".to_string(),
            size: 1,
            sha256: abc.to_string(),
        };
        assert!(matches!(
            check_runtime_files(dir.path(), &[missing], &mut hash_of),
            Err(RuntimeError::Missing { path }) if path == "SkyrimSE.exe"
        ));
        let traversal = RuntimeFile {
            path: "../Skyrim.esm".to_string(),
            size: 3,
            sha256: abc.to_string(),
        };
        assert!(matches!(
            check_runtime_files(dir.path(), &[traversal], &mut hash_of),
            Err(RuntimeError::Path {
                reason: PathError::DotSegment,
                ..
            })
        ));
        assert_eq!(asked, ["Data/Skyrim.esm", "Data/Skyrim.esm"]);
    }
}

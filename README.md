<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# sug-launcher

The launcher players use to join a Skyrim Underground game server. One launcher serves many servers: it lists the servers from a signed directory, lets a player add a server by URL, shows what each server's backend says about itself, installs the server's files into a game folder of its own, signs the player in to the server through Discord, and starts the game from that folder with a play token. It is a Tauri 2 desktop app for Windows: Rust for everything that touches the network, the disk or processes, and a plain HTML, CSS and JavaScript interface served from `ui/` as is.

## Status

The server browser, the install, the sign-in and Play. The launcher fetches the directory and checks its signature, keeps the last good copy when a fetch or a signature fails, merges the directory with servers added by URL, asks every server for its API version and marks the ones this launcher cannot use, polls every listed backend's `/api/v1/status` every 30 s while the list is open, shows a server's details, and keeps a redacted, rotating log. On a server's page it fetches the server's manifest, checks it against the rules below, lists the native code the server would install and where each file comes from, and installs or updates the server's game folder: the Steam copy of the game linked in, the manifest's files downloaded with resume and verified, files the manifest dropped removed, with progress, Cancel and Verify files. Files that come from an archive (`"kind": "archive"`) are parsed and checked but cannot be installed yet. The page also signs the player in to that server through Discord in the system browser, keeps the session for that server alone under DPAPI, shows whether the server lets the account play, and has Play: the files are checked, a play token is asked for, the Steam build is gated against the runtime the server announces, the server's hashed runtime files are checked in its game folder, and one game run swaps in the server's load order, writes the play token for the game client under DPAPI, starts the manifest's loader from the game folder and puts everything back when it exits or, after a crash, at the next start. Reconnect fetches a new token for a running game. Every refusal (banned, not a member, missing role, Discord unavailable, server offline or full, not signed in) is shown in plain words. At the start the launcher asks the website for the newest release, and a newer one is downloaded, its signature checked and its installer started before anything else. The signing key in `tauri.conf.json`, which verifies both the updates and the directory, is empty until the maintainer's release key exists, so a build from this tree checks for no updates, shows only servers added by URL, and says so.

## Prerequisites

- Windows 10 or 11 with the Microsoft Edge WebView2 Runtime (present on current Windows).
- Rust stable with the `x86_64-pc-windows-msvc` toolchain (`rustup`), 1.99 or newer.
- Visual Studio Build Tools with the "Desktop development with C++" workload (the MSVC linker and the Windows SDK); `rustc` finds them without a developer prompt.
- Node.js 24 and npm, for the Tauri CLI and the TypeScript checker. Neither is part of the launcher.

The first `npx tauri build` downloads NSIS into the Tauri CLI's own cache.

## Build

    npm ci
    npx tauri build --debug

The installer and the executable land under `src-tauri/target/debug/bundle/nsis/` and `src-tauri/target/debug/`. Without `--debug` the same command makes the release build. Both make an installer that is not signed for the updater; a release is built as described under "Releasing", with `src-tauri/release.conf.json` merged in, which makes the Tauri CLI sign the installer with the maintainer's key and refuse to build without it. To run the app while developing, `npx tauri dev` starts it straight from `ui/`; there is no bundler and no build step for the interface.

## Test

In `src-tauri/`:

    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

In the repository root:

    npm run typecheck
    npm test

`cargo test` runs the Rust unit tests and the integration tests in `src-tauri/tests/`, which start the Node servers in `test/` (`fake-backend.mjs` for the API routes, `static-files.mjs` for downloads), so Node must be on PATH. `npm test` runs the `node:test` suites in `test/`: the pure UI module and the two servers themselves. Tests that need a signed directory generate a throwaway minisign key in memory; no key is stored anywhere. Downloads and installs in tests go to temporary folders and a stand-in for the Steam copy. The game-run tests build a stub loader with `rustc` (found next to `cargo`) that records its working folder, its environment and its arguments, and use a `plugins.txt` and a launch file in temporary folders, so the real game profile is never touched. The DPAPI tests only prove the round trip for the user running them; that a file is unreadable under another Windows account is a check for a PC with two accounts. The sign-in tests play Discord and the browser themselves: they call the fake backend's callback with any code and follow its redirect to the launcher's loopback listener, so no browser opens and no Discord application is involved; the fake backend records the `Authorization` header of every request it serves (`GET /fake/log`), which is how the tests prove that each server's session is sent to that server's origin only. Sign-in and Play against a real backend are a check for the maintainer's PC once the backend's sign-in routes are live.

Two tests are skipped by default because they need the Steam copy of the game on the PC. One reads the build from the Steam copy's `SkyrimSE.exe`. The other builds a per-server folder from the Steam copy by hard links, replaces `Data/Skyrim.esm` in that folder with a small downloaded file, checks that the exe and the five masters in the Steam copy hash the same before and after, checks with `fsutil hardlink list` on the Steam copy's exe that the new folder's exe is one of its links, and checks the hashes again after the folder is deleted; it needs a scratch folder on the same volume as the Steam copy, which it removes when done. Both run with:

    set SUG_LAUNCHER_SCRATCH=C:\scratch\launcher-test
    cargo test the_steam_copy -- --ignored --nocapture

The self-update is tested against a `latest.json` and an installer served by `static-files.mjs`, through a mock Tauri app that carries the updater plugin and the updater section of `tauri.conf.json` with the key and the endpoint swapped, so the plugin's own check and download run without a window: a newer signed build is found and its download verified, an equal or lower version is no update, an installer that was tampered with, one signed with another key and one announced as a version it was not signed for are refused, a download that grows past what the launcher accepts and one cut short are refused, a missing or damaged `latest.json` and an unreachable host skip the check, and an empty key makes no check even when a newer release is announced. The installer's start is not run by a test, since on Windows it starts the installer and exits the process. One more test is skipped by default because it needs an installer signed by `npx tauri build`, with its `.sig` next to it, and the matching public key:

    set SUG_LAUNCHER_UPDATE_INSTALLER=C:\path\to\SUG Launcher_0.1.0_x64-setup.exe
    set SUG_LAUNCHER_UPDATE_PUBKEY=<the pubkey as tauri.conf.json carries it>
    cargo test the_installer_signed_by_the_build -- --ignored

It serves that installer from a temporary folder, announces the version named in its signature to a launcher that believes it runs version 0.0.1, and checks that the bytes come back verified and that a flipped bit in the installer is refused. Sign such a test build with a throwaway key (`npx tauri signer generate -w <a folder outside the repository>`) through a `--config` override that sets `bundle.createUpdaterArtifacts` to `true` and `plugins.updater.pubkey` to that key's public half, with `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` set; the throwaway key never enters the repository.

`test/fake-backend.mjs` is also handy on its own: `node test/fake-backend.mjs --port 4100 --api-version 1 --name "My Server"` answers `/api/v1/server`, `/api/v1/status` (`--offline` for an offline server, `--full` for a full one) and `/api/v1/manifest` (`--manifest <file>` serves a file, otherwise 503) on `http://127.0.0.1:4100`, plus the account routes: `POST /api/v1/users/login-discord/start`, `GET .../callback` (any `code` counts as Discord's; `--callback-error <code>` refuses instead), `POST .../redeem`, `GET /api/v1/users/me` and `POST /api/v1/play` (`--access ok|not_member|missing_role|banned|discord_unavailable`, `--ban-reason`, `--display-name`, `--game-host`, `--game-port`), `POST /api/v1/users/logout`, and `GET /fake/log`. `--authorize-url` changes the address the start route answers with, to show that the launcher refuses one that is not Discord's. `test/fixtures/manifest.json` is a manifest that passes every rule. The launcher itself only adds `https://` origins and only downloads from `https://` origins, so both servers serve the automated tests, not the launcher window.

## How it works

**Server list.** `servers.json` in the app config folder holds `{id, name, backendUrl, listed, addedAt}` per server and nothing else; description, links, runtime, protocol and status are fetched live from the backend each time, so a server owner changes them without the player doing anything. `id` is the backend host, plus the port when it is not the default one, and later names the server's game folder.

**Directory.** At the start and on Refresh the launcher fetches `directoryUrl` from `launcher.json` and the detached minisign signature next to it (`directory.json.minisig`), checks the signature with the public key in `tauri.conf.json`, then applies the list: directory entries replace the listed ones, a listed server the directory no longer names leaves the list and its stored sign-in goes with it, servers added by URL stay, and `servers.json` is rewritten only when the list changed. The verified bytes are kept in the cache folder; when a fetch or a signature fails, that last good copy is applied instead and the list says so. Coming back to the list from a server's page or from Settings shows what the last refresh found, without another fetch. The directory format is `{"schemaVersion": 1, "servers": [{"name": "...", "backendUrl": "https://..."}]}`, each `backendUrl` an https origin with no path.

**Add by URL.** The address must be an `https://` origin: no path, query, fragment, user name or password, and a host name that neither ends with a dot nor begins with a name Windows reserves for devices (`nul.example.org` would name the `NUL` device), since the host names the server's folders and files. The launcher fetches `/api/v1/server`, shows the name, description and website it answered with, and the warning that an unlisted server installs files, including native code, that its owner chooses. Only after the confirm is the entry stored, marked as added by URL; such an entry can be removed from its page.

**API version gate.** `/api/v1/server` carries `apiVersion`. This launcher knows version 1. At the start and on Refresh every server in the list is asked once; a higher version marks its row "needs a newer launcher", a lower one "server too old", the server's page says the same, and nothing else happens with that server. Every response from a backend is parsed into a typed shape and an unknown field is refused.

**HTTP.** One client with 10 s timeouts, no proxy from the environment, redirects followed only on the origin of the request, bodies capped (the manifest at 1 MiB). The launcher contacts every backend in the list for its API version at the start and on Refresh, every backend's status route while the list is open, the selected server's backend, the directory host, and the origins the selected server's manifest lists for its downloads; nothing else.

**Manifest.** `/api/v1/server` names the manifest route, which must sit on the backend's own origin. The manifest is JSON of at most 1 MiB and 20,000 files, `{schemaVersion: 1, version, gameRuntime, protocolId, runtime: {exe, files: [{path, size, sha256}]}, launch: {loader}, origins: [...], files: [{to, size, sha256, native, source}], plugins: [...]}`, with `source` either `{kind: "url", url}` or `{kind: "archive", page, archiveSha256, archiveSize, entry}`. A manifest that breaks any rule is refused as a whole and the page says which rule: every path (`to`, `path`, `exe`, `loader`, `entry`) is relative and `/`-separated, with no `.` or `..` segment, no empty segment, no drive, backslash, colon or control character, none of `<>"|?*`, no segment ending in a dot or a space, and no name Windows reserves for devices (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, whatever the extension); no `to` is named `installed.json` or ends in `.part`, the names the launcher keeps for its own record and its unfinished downloads; every `sha256` is 64 lower-case hex characters; `size` is present; `native` is `true` exactly when the extension is `exe`, `dll` or `asi`; every origin is a bare `https://` origin and every `url` and `page` sits on one of them; no path is listed twice (case does not matter) and no `to` repeats a runtime path; every plugin is a bare file name that `runtime.files`, `files` or the Steam copy's `Data` folder provides; `launch.loader` is an `.exe` that is either `runtime.exe` or a native file in `files`, since it is started as it is; `gameRuntime` equals what `/api/v1/server` announced; unknown fields are refused, except a `$comment`.

**Install.** Each server gets its own game folder, `<base>\servers\<id>\`. Every file of the Steam copy of the game that the manifest does not name is placed there as an NTFS hard link, or as a copy when the folder is on another volume; a file the manifest names is never linked, only downloaded. The Steam copy is found through the registry (`HKCU\Software\Valve\Steam`, then `HKLM\SOFTWARE\WOW6432Node\Valve\Steam`) and `steamapps\libraryfolders.vdf`, read only. The launcher never writes through a link: it only creates links and deletes them, and a downloaded file that replaces a vanilla path is written as `<name>.part` next to its place, verified, and renamed over the path after the link is deleted. Opening a server's page plans the install without writing anything; pressing Install creates the game folder and checks the free space on its volume against the bytes to download (plus the runtime files when they are copied) before any file is written. Downloads are streamed, hashed as they arrive, and abort the moment the host sends more than the size the manifest states; a download that stops early keeps its `.part` and the next install resumes it with a `Range` request; a `.part` that no longer matches is fetched whole; every redirect is followed by hand and must land on a listed origin. `installed.json` in the game folder records the manifest version and every file the launcher downloaded; on the next install the launcher removes only files in that record that the manifest no longer names, so files the player added stay. A file already in place is kept when its hash matches the manifest; hashes are cached in `hashes.json` under the app cache folder by size and modification time, and Verify files ignores the cache and hashes everything in full. Cancel is honoured between files and inside a download; the records are written however an install ends, so they describe the folder as it is.

**Base folder.** Settings holds the base folder; when it is empty the default is a folder named after the product at the root of the drive that holds the Steam copy (for example `C:\SUG Launcher`). A server's game folder must not sit inside the Steam copy of the game or contain it; a base folder that would put it there is refused on the server's page.

**Game build.** The server's page reads the `FileVersion` of the Steam copy's `SkyrimSE.exe` and names the build `skyrimse-<major>.<minor>.<build>`; a server whose `gameRuntime` is another build cannot be installed or played until it moves its manifest, and the page says which build each side has. Before a game starts, the manifest's `runtime.files` are checked in the server's game folder: each present, of the stated size and with the stated sha256, through the same hash cache the install uses, so a Steam update past the manifest or a damaged master is caught before the game sees it.

**Sign-in.** Once per server, through Discord in the system browser. The launcher makes a PKCE verifier (32 random bytes from the system's generator, base64url) and its S256 challenge, opens a listener on `127.0.0.1` with a port the system chooses, and sends `POST /api/v1/users/login-discord/start {challenge, port}`. The backend answers `{state, authorizeUrl, expiresIn}`; the launcher opens `authorizeUrl` only when it is `https://discord.com/oauth2/authorize` with a query (https, that host on its default port, that exact path, no credentials), otherwise the sign-in fails before any browser opens. The browser comes back to the backend's callback, which redirects it to `http://127.0.0.1:<port>/callback?state=...&code=...` (or `&error=...`). The listener answers only a request on `/callback` whose `state` is this flow's own and ignores every other request with a 404; a connection that sends no request within ten seconds is dropped, so it cannot hold up the browser's return; it closes after the first accepted request, or after five minutes, and the port is free again. With the code the launcher sends `POST .../redeem {state, code, verifier}` and stores the answer `{session, expiresAt, displayName}` as `sessions\<id>.bin` in the app data folder, DPAPI for the current user, one file per server named by the server's id. The session is sent as a `Bearer` header only to that server's origin (redirects to another origin are refused). `GET /api/v1/users/me` is asked every time the server's page opens and says whether the account may play: `ok`, `not_member`, `missing_role`, `banned` (with `banUntil` and a game ban's `banReason`) or `discord_unavailable`; a session the server no longer knows (401), a ban, Sign out (after `POST /api/v1/users/logout`), a session past its `expiresAt`, removing the entry and a listed server leaving the directory all delete the file. One sign-in waits for the browser at a time; Cancel closes its listener, and pressed before the server has answered the start request it also keeps the browser from opening.

**Play.** A press of Play runs a state machine through verifying files, requesting token, launching and running, and every step that fails returns to idle with its reason in plain words. Verifying files plans the install as the page does and refuses when anything is left to download, remove or place (the page's Install or Update button does that), then checks the manifest's hashed runtime files in the game folder. Requesting token sends `POST /api/v1/play` with the session and gets `{playToken, expiresAt, server: {host, port}}`, or a refusal: `banned` (the session is deleted), `not_member`, `missing_role`, `discord_unavailable`, `server_offline`, `server_full`, a session the server no longer knows (deleted, "sign in again"), or no session at all ("sign in first"). Launching is the game run below. While the game runs the launcher looks at the loader every half second; when it is gone, the files are put back and the run is idle again, with the exit code shown when it is not zero. Reconnect, shown while the game runs, asks for a new token the same way and writes a new launch file for the client to read at its next connect attempt.

**Play handoff.** One game run, driven by the play answer `{playToken, expiresAt, server: {host, port}}`: the player's `plugins.txt` (in `%LOCALAPPDATA%\Skyrim Special Edition\`, which the game reads whichever copy runs) is copied once to `plugins.txt.backup` next to it, the launch file is written, the server's load order is written over `plugins.txt` with every plugin enabled in the manifest's order behind a marker comment line, and the manifest's `launch.loader` is started with the server's game folder as its working folder, `-waitforclose` as its only argument and the launch file's path in the environment variable `SUG_LAUNCH_FILE`. The loader then lives as long as the game, so its exit is the game's: the launch file is deleted first, then the backup is renamed back over `plugins.txt`; a player who had no `plugins.txt` gets none back, since the marker says the file was the launcher's. A read-only `plugins.txt` is made writable for the run and comes back read-only, because the backup keeps the file's attributes. A reconnect writes a new launch file while the game runs. If anything fails between the first write and the loader's start, the files are put back before the error is shown. A run that did not end through the launcher (a crash, or the launcher closed) leaves those files behind, and the next start removes the stale launch file before anything else and then puts `plugins.txt` back the same way.

The launch file, `launch.bin` in the app data folder, is the whole contract with the game client: DPAPI (`CryptProtectData`, current user, no extra entropy, no prompt) over the UTF-8 JSON `{"schemaVersion": 1, "host": "...", "port": 7777, "playToken": "sugp_...", "expiresAt": 1760000000000}`, with `expiresAt` as the backend sent it, milliseconds since the Unix epoch. The client finds the path in `SUG_LAUNCH_FILE`, inherited through the loader, and unwraps the file with `CryptUnprotectData` at each connect attempt; only the same Windows user can.

**Update.** At the start, before the list is loaded, the launcher asks the endpoint in `tauri.conf.json` (`plugins.updater.endpoints`, the website's `launcher/latest.json`) for the newest release and compares its version with its own. A newer one is downloaded, its minisign signature checked against `plugins.updater.pubkey`, and its installer started, on an update screen that is the only thing shown until the installer runs or the update fails; the server list, the status polling, installs, sign-ins and Play all wait behind it. The installer is the per-user NSIS installer of that release, run in passive mode (a progress bar and nothing to click) with the updater's `/UPDATE` switch; the launcher exits as soon as the installer runs, and the installer starts the new version when it is done. The `.sig` file next to each installer names the version it was signed for in its trusted comment, and the updater refuses a download whose announced version differs from that or whose signature names none, so a `latest.json` that pairs a new version number with an older installer is refused too. A check that cannot be made (no network, no release published yet, an empty key) is noted above the list and the launcher goes on as it is; a download or an installer start that fails says so on the update screen and offers to continue without updating; the next start checks again. The check answers within 10 s; a download is given up when it sends nothing for 30 s, when it grows past 64 MiB, or when it is not finished after 10 minutes. The update host sees the launcher's user agent (product and version) and the player's address, nothing else, and the log notes the check's outcome and a failure's reason without URLs.

**Log.** `launcher.log` in the app log folder, rotated at 1 MiB with four older copies kept. Every line is redacted before it is written: a session (`sugs_...`) or play token (`sugp_...`) keeps only its prefix, and a URL keeps only its scheme and host. No line names a session, a token, a sign-in state, a login code or the player's Discord identity; the display name appears only in the window.

**Settings.** `settings.json` in the app config folder holds the base folder under which every server will get its own game folder.

**Files the launcher cannot read.** A `servers.json` or `settings.json` that is damaged or carries another schema version (for example one written by a newer launcher) is renamed to `.bad` next to its place, the launcher starts over without it, and the list says so once. Only a failure to create the app folders themselves stops the launcher; that is shown in a message box, since a release build has no console.

Folders on Windows, named after the identifier in `tauri.conf.json`: config, `launch.bin` and `sessions\<id>.bin` in `%APPDATA%\org.skyrimunderground.launcher\`, the directory cache and each server's `hashes.json` in `%LOCALAPPDATA%\org.skyrimunderground.launcher\cache\`, logs in `%LOCALAPPDATA%\org.skyrimunderground.launcher\logs\`. The game's own `plugins.txt` and its backup are the only files the launcher writes outside its folders and the base folder.

**What leaves the PC.** The selected server's backend sees the sign-in requests, the session (as a header to that origin only), the Play requests and so the player's address; Discord sees the sign-in the player makes in the browser; the website sees the directory and update checks, and the release host sees the download of an update. Nothing else: no analytics, no hardware identifier, no crash reports.

## The two files a fork edits

- `src-tauri/tauri.conf.json`: product name, window title, bundle identifier, the update endpoint (`plugins.updater.endpoints`, the `latest.json` described under "Releasing") and the public key (`plugins.updater.pubkey`) that verifies both the updates and the directory. Generate the key pair on a machine you trust with `npx tauri signer generate -w <path outside the repository>`; it prints the public key as one base64 string, which goes into `pubkey`. The private key never enters a repository or a build machine.
- `launcher.json`: `directoryUrl` (the signed directory), `website` and `rulesUrl` (the page that states when a server is removed from the directory). All three must be https.

To publish a directory, sign the file with that key pair in minisign's format: the `tauri signer` key file is a minisign secret key encoded in base64, so decode it once into a `minisign.key` and run `minisign -Sm directory.json -s minisign.key`, then upload `directory.json` and `directory.json.minisig` to `directoryUrl`. The signature is checked in minisign's current, prehashed mode.

## Releasing

The maintainer's PC holds the only signing key, made once with `npx tauri signer generate -w <a folder outside every repository>\sug-launcher.key` and a password. The public half (printed, and in the `.pub` file next to the key) goes into `src-tauri/tauri.conf.json` as `plugins.updater.pubkey` and is committed; the private half and its password stay on that PC, backed up where the maintainer keeps secrets, and never reach the game box, a CI runner or a repository. Losing the key means installed launchers can no longer update: a new key needs a fresh install from the website. The same key signs the directory.

For each release:

1. Set the new version in `src-tauri/Cargo.toml` and `package.json`, commit, and tag `v<version>`.
2. In a shell with `TAURI_SIGNING_PRIVATE_KEY` set to the key file's path and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to its password, run `npm ci` and then `npx tauri build --config src-tauri/release.conf.json`. The override turns on `bundle.createUpdaterArtifacts`, so the CLI signs the installer after checking that the private key matches `pubkey`, and the build fails without the key. The result is `src-tauri/target/release/bundle/nsis/SUG Launcher_<version>_x64-setup.exe` with `SUG Launcher_<version>_x64-setup.exe.sig` next to it, the signature in base64 with the version in its trusted comment.
3. Make the GitHub release for the tag and attach both files. GitHub turns the spaces in a file name into dots, so copy the installer's download link from the release page.
4. In sug-website, write `launcher/latest.json` so it is served at the endpoint in `tauri.conf.json`:

        {
          "version": "<version>",
          "notes": "What changed, in a sentence.",
          "pub_date": "2026-10-14T12:00:00Z",
          "platforms": {
            "windows-x86_64": {
              "signature": "<the content of the .sig file, one line>",
              "url": "https://github.com/<owner>/sug-launcher/releases/download/v<version>/SUG.Launcher_<version>_x64-setup.exe"
            }
          }
        }

   `version` is the one the installer was signed for, `signature` is the `.sig` file as it is, `url` is the installer's download link, and `notes` and `pub_date` are optional. Deploy the website; every launcher picks the release up at its next start, and the website's download link for new players points at the same installer.

A debug build for testing is signed the same way with a throwaway key, as described under "Test"; a plain `npx tauri build` makes an installer that is not signed, which a launcher never installs as an update.

## Layout

    launcher.json                the directory URL, website and rules link
    ui/                          the interface, served as is (ES modules typed with JSDoc, checked by tsc)
      index.html, styles.css     the four screens, the account, files and Play panels, and the add dialog
      app.js                     entry point and screen switching
      backend.js                 typed wrappers over the Rust commands and their JSON contracts
      gate.js                    presentation rules without DOM (tested by test/gate.test.mjs)
      list.js, server.js, add.js, settings.js, update.js   one module per screen
      account.js, play.js        the sign-in panel and the Play panel of a server's page
      dom.js                     DOM helpers; server strings reach the page through textContent only
      globals.d.ts               the part of the Tauri global the UI uses
    src-tauri/
      tauri.conf.json            branding, window, CSP, bundle, the update endpoint and the signing key
      release.conf.json          merged into tauri.conf.json for a release build: the installer is signed
      capabilities/default.json  the main window's permissions: none beyond the launcher's own commands
      icons/                     made with "npx tauri icon" from a neutral image
      src/main.rs                the executable
      src/lib.rs                 the Tauri builder, plugins, managed state, the commands and the start failure box
      src/config.rs              launcher.json, the signing key, app folders, settings.json, the default base folder
      src/log.rs                 the log file: rotation and redaction
      src/update.rs              the self-update: the check at start, the download, the signature check and the installer
      src/api.rs                 the HTTP client, the /api/v1 shapes, the version gate, the manifest fetch and the account routes
      src/signin.rs              the PKCE pair, the loopback listener, the authorize address check and one sign-in flow
      src/play.rs                the Play state machine, pure
      src/servers.rs             servers.json, the directory fetch and signature check, add-by-URL rules
      src/manifest.rs            the manifest's shape and every rule it must pass
      src/download.rs            streamed downloads: .part files, Range resume, hashing, the size cap, origin checks
      src/deploy.rs              the per-server folder: links or copies, placing and removing files, both records, the runtime check
      src/game.rs                the Steam copy of the game, its build from the exe's FileVersion, the runtime checks, plugins.txt's place
      src/secrets.rs             DPAPI for the current user, protected files, the system random bytes and the per-server session files
      src/launch.rs              one game run: plugins.txt backup and restore, the launch file, the loader process, recovery at start
      src/test_support.rs        a canned HTTP server for tests
      tests/common/mod.rs        starts the Node servers for the integration tests
      tests/fake_backend.rs      the Rust side against the Node fake backend
      tests/signin.rs            sign-in and Play against the fake backend, the test playing Discord and the browser
      tests/install.rs           downloads and installs against the static file server, and the Steam copy when asked
      tests/launch.rs            a game run against a stub loader built by rustc in the test
      tests/update.rs            the self-update against a latest.json and an installer on the static file server
    test/
      fake-backend.mjs           a stand-in backend serving the public routes, the sign-in, me, logout and play
      static-files.mjs           a file server with Range support and routes that cut, never stop or redirect
      spawn.mjs                  starts either server for the node:test suites
      fixtures/directory.json    a directory, signed in tests with a throwaway key
      fixtures/manifest.json     a manifest that passes every rule
      gate.test.mjs, fake-backend.test.mjs, static-files.test.mjs   node:test suites

## Licence

AGPL-3.0-or-later; see `LICENSE` and `NOTICE`. Contributions need a DCO sign-off; see `CONTRIBUTING.md`.

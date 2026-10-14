<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Contributing

## Sign-off

Every commit carries a `Signed-off-by:` line, added with `git commit -s`. By signing off you certify the [Developer Certificate of Origin 1.1](https://developercertificate.org/): you wrote the change, or otherwise have the right to submit it under this repository's licence. There is no CLA, and contributors keep the copyright on their contributions.

## Licence

Contributions are licensed under AGPL-3.0-or-later (see `LICENSE`; the scope is in `NOTICE`). New source files start with an `SPDX-License-Identifier: AGPL-3.0-or-later` comment where the file format allows comments. JSON files that allow extra keys carry it as a first `"$comment"` key instead (see `package.json`); generated files such as `package-lock.json` and `Cargo.lock`, and `tauri.conf.json` and `release.conf.json`, whose schema allows no extra keys, carry none.

## Code from other projects

Code taken from another project keeps its original licence notices and is listed in `NOTICE` with its source and licence.

## Checks before a pull request

In `src-tauri/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass. In the repository root: `npm ci`, `npm run typecheck` and `npm test` must pass. No secret, token, session value or full download URL may appear in code, tests, logs or committed config; the only signing key a test ever uses is generated inside the test, or, for the one update test that is skipped by default, kept outside the repository and named by an environment variable.

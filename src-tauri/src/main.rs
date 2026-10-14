// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if sug_launcher_lib::run().is_err() {
        std::process::exit(1);
    }
}

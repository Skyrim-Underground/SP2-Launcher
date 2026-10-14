// SPDX-License-Identifier: AGPL-3.0-or-later
use std::env;
use std::fs;
use std::path::PathBuf;

/// The dialog code a test app links needs Common Controls 6, which Windows grants only through a manifest.
const TEST_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    tauri_build::build();
    embed_test_manifest();
}

/// The executable gets its manifest from tauri_build; test binaries get the same dependency, or a mock app cannot start.
fn embed_test_manifest() {
    let Some(out_dir) = env::var_os("OUT_DIR") else {
        return;
    };
    let path = PathBuf::from(out_dir).join("test-manifest.xml");
    if let Err(error) = fs::write(&path, TEST_MANIFEST) {
        panic!("the test manifest could not be written: {error}");
    }
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
        path.display()
    );
}

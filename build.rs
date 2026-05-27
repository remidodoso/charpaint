// build.rs — runs on the host at compile time, never included in the WASM output.
// 1. Passes the build timestamp into the binary as BUILD_TIMESTAMP env var.
// 2. Rewrites the `<!-- build: ... -->` comment in index.html so the file content
//    changes on every build, guaranteeing the browser gets a 200 (not a 304) on
//    reload and picks up the fresh WASM/JS.
fn main() {
    let now = chrono::Local::now();
    let stamp = now.format("%m%d:%H%M").to_string();

    // Pass timestamp into the Rust binary.
    println!("cargo:rustc-env=BUILD_TIMESTAMP=Build: {stamp}");

    // Rewrite the build comment in index.html.
    let path = "index.html";
    if let Ok(content) = std::fs::read_to_string(path) {
        let updated = content
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("<!-- build:") {
                    format!("<!-- build: {stamp} -->")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Preserve a trailing newline if the original had one.
        let updated = if content.ends_with('\n') {
            updated + "\n"
        } else {
            updated
        };
        let _ = std::fs::write(path, updated);
    }

    // ── Help strings codegen ──────────────────────────────────────────────────
    // Read locales/help.en.yaml and emit a `help()` function into OUT_DIR.
    // The generated file is included by src/lib.rs via include!().
    // No rerun-if-changed → build script always runs, so strings stay in sync.
    {
        use std::collections::BTreeMap;

        let yaml_src = std::fs::read_to_string("locales/help.en.yaml")
            .unwrap_or_default();
        let map: BTreeMap<String, String> = serde_yaml::from_str(&yaml_src)
            .unwrap_or_default();

        let mut arms = String::new();
        for (key, val) in &map {
            // Escape backslashes and double-quotes so the value is valid in a Rust string literal.
            let escaped = val.replace('\\', "\\\\").replace('"', "\\\"");
            arms.push_str(&format!("        \"{key}\" => Some(\"{escaped}\"),\n"));
        }

        let code = format!(
            "/// Look up a help string by its data-help key.\n\
             /// Returns None for unknown keys; callers may show a fallback message.\n\
             pub(crate) fn help(key: &str) -> Option<&'static str> {{\n\
             \x20\x20\x20\x20match key {{\n\
             {arms}\
             \x20\x20\x20\x20\x20\x20\x20\x20_ => None,\n\
             \x20\x20\x20\x20}}\n\
             }}\n"
        );

        let out_dir  = std::env::var("OUT_DIR").unwrap();
        let out_path = std::path::Path::new(&out_dir).join("help_strings.rs");
        std::fs::write(&out_path, code).unwrap();
    }

    // No rerun-if-changed directive → build script runs on every `cargo build`,
    // so the timestamp is always fresh and the YAML is always re-read.
}

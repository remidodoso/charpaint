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

    // No rerun-if-changed directive → build script runs on every `cargo build`,
    // so the timestamp is always fresh.
}

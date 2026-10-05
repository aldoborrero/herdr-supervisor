use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// No rerun-if-changed lines on purpose: cargo then reruns this script whenever
// any file in the package changes, so a rebuilt binary always gets a new build id.
fn main() {
    // Nix supplies a content-addressed source ID; never include wall-clock time there.
    println!("cargo:rerun-if-env-changed=HERDR_SUPERVISOR_BUILD_ID");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if let Ok(id) = std::env::var("HERDR_SUPERVISOR_BUILD_ID") {
        println!("cargo:rustc-env=HP_BUILD_ID={id}");
        return;
    }
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nogit".to_string());
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=HP_BUILD_ID={hash}.{secs}");
}

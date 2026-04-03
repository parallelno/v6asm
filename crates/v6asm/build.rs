use std::process::Command;

use chrono::Local;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let date = Local::now().format("%Y.%m.%d").to_string();
    let hash = git_short_hash().unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=V6ASM_VERSION={}-{}", date, hash);
}

fn git_short_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let hash = String::from_utf8(output.stdout).ok()?;
    let hash = hash.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}
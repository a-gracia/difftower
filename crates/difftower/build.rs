//! Builds `difftower-agent` as a static Linux binary and embeds it, so the
//! app can install the matching agent on any remote host it connects to.

use std::{env, path::PathBuf, process::Command};

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap().to_path_buf();
    // A separate target directory: the outer build holds a lock on its own.
    let target_dir = workspace.join("target").join("agent");

    let status = Command::new(env::var("CARGO").unwrap())
        .current_dir(&workspace)
        .args(["build", "--release", "--locked", "-p", "difftower-agent", "--target", TARGET])
        .arg("--target-dir")
        .arg(&target_dir)
        // Static glibc: the agent must run on hosts with any C library version.
        .env("CARGO_ENCODED_RUSTFLAGS", "-Ctarget-feature=+crt-static")
        .env_remove("RUSTFLAGS")
        .status()
        .expect("failed to run cargo for difftower-agent");
    assert!(status.success(), "building difftower-agent failed");

    let agent = target_dir.join(TARGET).join("release").join("difftower-agent");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("difftower-agent");
    std::fs::copy(&agent, &out).expect("failed to copy difftower-agent");

    for path in ["crates/difftower-agent", "crates/difftower-core", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(path).display());
    }
}

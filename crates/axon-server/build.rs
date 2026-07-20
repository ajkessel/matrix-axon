use std::env;
use std::process::Command;

fn main() {
    // Re-run this script when GIT_HASH changes. Without it, Cargo's default
    // heuristic only re-runs build.rs when a file inside this crate changes — so
    // with the persistent `target/` cache mount the image build uses
    // (deploy/Dockerfile, and repeat `publish.sh` runs), a new `--build-arg
    // GIT_HASH` would NOT re-run build.rs and the pushed image could keep a stale
    // AXON_GIT_HASH. Harmless locally, where GIT_HASH is normally unset.
    println!("cargo:rerun-if-env-changed=GIT_HASH");

    // Prefer an explicit GIT_HASH from the environment (e.g. a Docker build-arg)
    // before the git/jj subprocesses: the image build context excludes `.git`
    // (and has no `jj`), so those would always fail there and stamp "unknown".
    let hash = env::var("GIT_HASH")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            Command::new("jj")
                .args(["log", "-r", "@", "--no-graph", "-T", "commit_id.short()"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let rust = Command::new("rustc")
        .args(["--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());

    let build_time = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let build_info = format!("{version}-{hash}-{profile}-{build_time} / {rust}");

    println!("cargo:rustc-env=BUILD_INFO={build_info}");
    println!("cargo:rustc-env=AXON_GIT_HASH={hash}");
    println!("cargo:rustc-env=AXON_PROFILE={profile}");
    println!("cargo:rustc-env=AXON_BUILD_TIME={build_time}");
    println!("cargo:rustc-env=AXON_RUSTC_VERSION={rust}");
}

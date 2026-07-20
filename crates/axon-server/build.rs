use std::env;
use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .or_else(|| {
            Command::new("jj")
                .args(["log", "-r", "@", "--no-graph", "-T", "commit_id.short()"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
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

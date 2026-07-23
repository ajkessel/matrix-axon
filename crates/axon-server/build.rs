use std::env;
use std::process::Command;

fn main() {
    // Re-run this script when GIT_HASH changes (the persistent `target/` cache
    // mount the image build uses — deploy/Dockerfile, repeat `publish.sh` runs —
    // would otherwise keep a stale AXON_GIT_HASH across a new `--build-arg
    // GIT_HASH`), AND whenever any workspace crate's source changes — not just
    // this one. `axon-server` is a thin composition root; almost all real work
    // happens in its dependencies (axon-sync, axon-api, …), and *that* changing
    // still relinks the `axon` binary into a genuinely new artifact. Watching
    // only `src` (this crate's own tree) missed exactly that case: the binary
    // got rebuilt but this script didn't rerun, so it kept stamping the first
    // build's git_hash/build_time all day. `../` is `crates/` (recursed), which
    // covers every workspace member's source in one watch.
    println!("cargo:rerun-if-env-changed=GIT_HASH");
    println!("cargo:rerun-if-changed=../");

    // Prefer an explicit GIT_HASH from the environment (e.g. a Docker build-arg)
    // before the jj/git subprocesses: the image build context excludes `.git`
    // (and has no `jj`), so those would always fail there and stamp "unknown".
    //
    // jj before git: this repo is colocated (a real `.git` dir jj keeps in
    // sync), so `git rev-parse HEAD` always succeeds there too — but it only
    // reflects the last commit jj has *exported*, not jj's working-copy commit
    // (`@`), which auto-snapshots on every `jj` invocation and so reflects
    // whatever's on disk right now, including edits not yet `jj commit`ed. For a
    // jj-managed checkout, `@` is the more accurate answer; git is the fallback
    // for a plain (non-jj) clone or CI checkout that has no `.jj`.
    let hash = env::var("GIT_HASH")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
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

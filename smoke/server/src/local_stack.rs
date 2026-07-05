use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};

use crate::wire::Manifest;

pub struct LocalStack {
    pub manifest_path: PathBuf,
    pub manifest: Manifest,
    bin: PathBuf,
    owns_stack: bool,
    keep_up: bool,
    torn_down: Cell<bool>,
}

impl LocalStack {
    pub fn up(run_id: &str, manifest_path: PathBuf, keep_up: bool) -> anyhow::Result<Self> {
        let bin = resolve_local_stack_bin()?;
        let mut cmd = Command::new(&bin);
        cmd.args([
            "up",
            "--manifest",
            manifest_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("manifest path is not UTF-8"))?,
            "--run-id",
            run_id,
            "--quiet",
        ]);
        if keep_up {
            cmd.arg("--keep-up");
        }
        let status = cmd.status().context("run axon-smoke-local-stack up")?;
        if !status.success() {
            bail!("axon-smoke-local-stack up failed with {status}");
        }
        let manifest = read_manifest(&manifest_path)?;
        Ok(Self {
            manifest_path,
            manifest,
            bin,
            owns_stack: true,
            keep_up,
            torn_down: Cell::new(false),
        })
    }

    pub fn attach(manifest_path: PathBuf) -> anyhow::Result<Self> {
        let bin = resolve_local_stack_bin()?;
        let manifest = read_manifest(&manifest_path)?;
        Ok(Self {
            manifest_path,
            manifest,
            bin,
            owns_stack: false,
            keep_up: true,
            torn_down: Cell::new(false),
        })
    }

    pub fn owns_stack(&self) -> bool {
        self.owns_stack
    }

    pub fn down(&self) -> anyhow::Result<()> {
        // Mark first so a later failure (or the Drop guard) never retries a
        // teardown we already attempted, and so keep-up/attach modes are
        // recorded as intentionally-not-torn-down.
        self.torn_down.set(true);
        if !self.owns_stack || self.keep_up {
            eprintln!(
                "smoke(server): keeping local stack up; manifest {}",
                self.manifest_path.display()
            );
            return Ok(());
        }
        let status = Command::new(&self.bin)
            .args([
                "down",
                "--manifest",
                self.manifest_path.to_string_lossy().as_ref(),
            ])
            .status()
            .context("run axon-smoke-local-stack down")?;
        if !status.success() {
            bail!("axon-smoke-local-stack down failed with {status}");
        }
        Ok(())
    }
}

impl Drop for LocalStack {
    /// Safety net: if the run returns before `down()` was ever called (e.g. a
    /// fallible step after `up()`/`attach()` fails), tear an owned stack down on
    /// drop so Docker resources aren't leaked. `down()` itself no-ops for
    /// attach/keep-up modes, so this never tears down a stack we don't own.
    fn drop(&mut self) {
        if self.torn_down.get() {
            return;
        }
        if let Err(err) = self.down() {
            eprintln!("smoke(server): stack teardown on drop failed: {err:#}");
        }
    }
}

fn read_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn resolve_local_stack_bin() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("AXON_SMOKE_LOCAL_STACK_BIN") {
        return Ok(std::fs::canonicalize(PathBuf::from(path))?);
    }
    eprintln!("smoke: building axon-smoke-local-stack (set AXON_SMOKE_LOCAL_STACK_BIN to skip)");
    let status = Command::new(env_cargo())
        .args(["build", "-p", "axon-smoke-local-stack"])
        .status()
        .context("cargo build -p axon-smoke-local-stack")?;
    if !status.success() {
        bail!("cargo build -p axon-smoke-local-stack failed");
    }
    let bin = target_dir()?.join("debug").join(if cfg!(windows) {
        "axon-smoke-local-stack.exe"
    } else {
        "axon-smoke-local-stack"
    });
    if !bin.exists() {
        bail!("local-stack binary not found at {}", bin.display());
    }
    Ok(bin)
}

fn env_cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn target_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(workspace_root()?.join("target"))
}

fn workspace_root() -> anyhow::Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("cannot derive workspace root from {}", manifest.display()))
}

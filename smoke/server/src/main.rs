//! `axon-smoke-server` — black-box HTTP/WebSocket smoke harness for
//! `axon-server`.
//!
//! ```sh
//! cargo run -p axon-smoke-server -- --profile true-local [--filter NAME]
//! ```

mod axon;
mod local_stack;
mod matrix;
mod redact;
mod runner;
mod scenarios;
mod util;
mod wire;

use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    profile: String,
    filter: Option<String>,
    manifest: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut profile = None;
    let mut filter = None;
    let mut manifest = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => {
                profile = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--profile requires a value"))?,
                );
            }
            "--filter" => {
                filter = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--filter requires a value"))?,
                );
            }
            "--manifest" => {
                manifest =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--manifest requires a value")
                    })?));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: axon-smoke-server --profile true-local [--filter NAME] [--manifest PATH]"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(Args {
        profile: profile.unwrap_or_else(|| "true-local".to_owned()),
        filter,
        manifest,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("smoke(server): {err:#}");
            return ExitCode::from(2);
        }
    };

    match runner::run(&args.profile, args.filter.as_deref(), args.manifest).await {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("smoke(server): fatal: {err:#}");
            ExitCode::from(2)
        }
    }
}

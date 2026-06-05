use std::env;

use uuid::Uuid;

#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) base_url: String,
    pub(crate) account_id: Option<Uuid>,
}

impl Args {
    pub(crate) fn parse() -> anyhow::Result<Self> {
        let mut base_url = "http://127.0.0.1:8080".to_owned();
        let mut account_id = None;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--base-url" => {
                    base_url = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--base-url requires a value"))?;
                }
                "--account-id" => {
                    let value = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--account-id requires a value"))?;
                    account_id = Some(value.parse()?);
                }
                "--help" | "-h" => {
                    println!("Usage: axon-tui [--base-url URL] [--account-id UUID]");
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(Self {
            base_url,
            account_id,
        })
    }
}

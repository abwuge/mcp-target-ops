mod core;
mod protocol;
mod tooling;
mod transport;

use crate::core::{config::Config, error::Result, state::AppState};
use std::{path::PathBuf, sync::Arc};

const HELP: &str = r#"Usage: mcp-target-ops [OPTIONS]

Options:
  -c, --config PATH    TOML configuration path
      --http ADDR      Listen over HTTP instead of stdio
  -V, --version        Print version information
  -h, --help           Print this help

Long options also accept --config=PATH, --http=ADDR, and --http-addr=ADDR.
Without --config, MCP_TARGET_OPS_CONFIG or ~/.config/mcp-target-ops/config.toml is used when present."#;

struct Args {
    config_path: Option<PathBuf>,
    http_addr: Option<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("mcp-target-ops failed: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args();
    let config = Config::load(args.config_path)?;
    let state = Arc::new(AppState::new(config)?);

    if let Some(addr) = args.http_addr {
        protocol::http::serve_http(state, &addr)
    } else {
        protocol::mcp::serve_stdio(state)
    }
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        config_path: None,
        http_addr: None,
    };

    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--config=") {
            parsed.config_path = Some(PathBuf::from(value));
            continue;
        }
        // COMPAT(COMPAT-007): --http-addr is the older spelling retained for
        // existing service files and scripts; --http is the canonical flag.
        if let Some(value) = arg
            .strip_prefix("--http=")
            .or_else(|| arg.strip_prefix("--http-addr="))
        {
            parsed.http_addr = Some(value.to_string());
            continue;
        }

        match arg.as_str() {
            "--config" | "-c" => {
                parsed.config_path = Some(PathBuf::from(next_arg(&mut args, &arg)));
            }
            "--http" | "--http-addr" => {
                // COMPAT(COMPAT-007): Keep the spaced --http-addr form in sync
                // with the =value alias above until the old CLI spelling is retired.
                parsed.http_addr = Some(next_arg(&mut args, &arg));
            }
            "--help" | "-h" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("mcp-target-ops {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}\nTry 'mcp-target-ops --help'.");
                std::process::exit(2);
            }
        }
    }

    parsed
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("missing value for {flag}");
        std::process::exit(2);
    })
}

//! Command dispatch for the `stockhouse` binary.

mod args;
mod commands;

use std::io::Write;
use std::process::ExitCode;

pub use args::{Parsed, UsageError, parse};
pub use commands::guard_hook;

/// Exit status for usage and configuration errors.
pub const EXIT_USAGE: u8 = 2;

const USAGE: &str = "usage: stockhouse <command>
  proxy --config <file>
  uploader --config <file> [--once]
  post-build-hook --queue <dir>
  status --queue <dir>
  login | logout | refresh
  --version";

/// The binary entry point.
pub fn main(args: Vec<String>) -> ExitCode {
    let env = |name: &str| std::env::var(name).ok();
    ExitCode::from(run(&args, &env, &mut std::io::stdout(), &mut std::io::stderr()))
}

/// Runs a command with injected environment and output streams.
pub fn run(args: &[String], env: &dyn Fn(&str) -> Option<String>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match args::parse(args.get(1..).unwrap_or_default()) {
        Ok(parsed) => commands::dispatch(parsed, env, out, err),
        Err(error) => {
            let _ = writeln!(err, "stockhouse: {error}\n{USAGE}");
            EXIT_USAGE
        }
    }
}

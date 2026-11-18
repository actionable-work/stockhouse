//! The entry crate of `stockhouse-fake-server`, the NixOS VM stories' local
//! fixture servers.
//!
//! Test code only. `oauth` serves the shared `stockhouse-oauth-fake`
//! handlers with a private cache directory, `upload` a disk-backed fake of
//! upload and lease protocol v1 with control files, and `shared` the
//! shared-tier fixture. None is a model of any real server, and the flake
//! never installs this binary outside the tests.

mod args;
mod http;
mod ledger;
mod shared;
mod upload;

pub use args::{Mode, Usage, parse};

use std::process::ExitCode;
use std::time::Duration;
use tokio::net::TcpListener;

const USAGE: &str = "usage: stockhouse-fake-server oauth --listen <addr> [--cache-dir <dir>]
       stockhouse-fake-server upload --listen <addr> --root <dir> --token-file <file>
       stockhouse-fake-server shared --listen <addr> --log <file> [--deny <hash>] [--fail <hash>]";

/// Runs the fake server: 2 on a usage error, 1 when it cannot bind or
/// start, and otherwise serves until the process is stopped.
pub fn main(args: Vec<String>) -> ExitCode {
    let mode = match parse(args.get(1..).unwrap_or_default()) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("stockhouse-fake-server: {usage}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        eprintln!("stockhouse-fake-server: cannot start the runtime");
        return ExitCode::FAILURE;
    };
    runtime.block_on(async {
        match TcpListener::bind(mode.listen()).await {
            Ok(listener) => {
                mode.serve(listener).await;
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("stockhouse-fake-server: cannot listen: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

impl Mode {
    /// Serves this mode on `listener` until the process ends (it returns
    /// only if the upload root cannot be prepared).
    pub async fn serve(self, listener: TcpListener) {
        match self {
            Self::OAuth { cache_dir, .. } => {
                let mut state = stockhouse_oauth_fake::State::default();
                state.auto_approve = false;
                state.refresh_delay = Duration::from_millis(200);
                state.cache_dir = cache_dir;
                state.scope = "cache:read".to_owned();
                if let Ok(base) = listener.local_addr() {
                    state.verification_uri = format!("http://{base}/device");
                }
                stockhouse_oauth_fake::serve_forever(listener, state).await;
            }
            Self::Upload {
                root, token_file, ..
            } => upload::serve(listener, root, token_file).await,
            Self::Shared {
                log, deny, fail, ..
            } => shared::serve(listener, log, deny, fail).await,
        }
    }
}

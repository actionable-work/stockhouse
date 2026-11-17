use crate::EXIT_USAGE;
use crate::args::Parsed;
use std::io::Write;
use std::panic::{AssertUnwindSafe, UnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use stockhouse_credentials::{DeferredGrantFlow, GrantFlow};
use stockhouse_ingest::{
    BackendConfig, DeferredUploader, NixCopyUploader, NixPathInfoScanner, NoScanner, Queue,
    Scanner, ScannerConfig, Uploader, UploaderConfig, Worker, hook,
};
use stockhouse_proto::PROTOCOL_VERSION;
use stockhouse_proxy::ProxyConfig;

const EXIT_FAILURE: u8 = 1;

pub(crate) fn dispatch(
    parsed: Parsed,
    env: &dyn Fn(&str) -> Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match parsed {
        Parsed::Version => {
            let _ = writeln!(
                out,
                "stockhouse {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            0
        }
        Parsed::Grant(command) => grant(command, err),
        Parsed::Status { queue } => status(&queue, out, err),
        Parsed::Hook { queue } => {
            let out_paths = env("OUT_PATHS").unwrap_or_default();
            guard_hook(AssertUnwindSafe(|| {
                hook::run(&queue, &out_paths, now(), err);
            }))
        }
        Parsed::Proxy { config } => proxy(&config, err),
        Parsed::Uploader { config, once } => uploader(&config, once, err),
    }
}

/// Runs hook work and returns 0 whatever happens, including a panic.
pub fn guard_hook<F: FnOnce() + UnwindSafe>(work: F) -> u8 {
    let _ = catch_unwind(work);
    0
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn grant(command: &str, err: &mut dyn Write) -> u8 {
    let flow = DeferredGrantFlow;
    let result = match command {
        "login" => flow.login(),
        "logout" => flow.logout(),
        _ => flow.refresh(),
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "stockhouse {command}: {error}");
            EXIT_FAILURE
        }
    }
}

fn status(queue: &Path, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match Queue::open(queue).and_then(|q| q.status()) {
        Ok(status) => {
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string(&status).expect("status serializes")
            );
            0
        }
        Err(error) => {
            let _ = writeln!(err, "stockhouse status: {error}");
            EXIT_FAILURE
        }
    }
}

fn read_config(path: &Path, err: &mut dyn Write) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) => {
            let _ = writeln!(
                err,
                "stockhouse: cannot read configuration {}: {error}",
                path.display()
            );
            None
        }
    }
}

fn proxy(path: &Path, err: &mut dyn Write) -> u8 {
    let Some(text) = read_config(path, err) else {
        return EXIT_USAGE;
    };
    let config = match ProxyConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(err, "stockhouse proxy: {error}");
            return EXIT_USAGE;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime starts");
    runtime.block_on(async {
        match stockhouse_proxy::bind(&config, stockhouse_proxy::stderr_log()).await {
            Ok(server) => {
                server.run().await;
                0
            }
            Err(error) => {
                let _ = writeln!(err, "stockhouse proxy: {error}");
                EXIT_FAILURE
            }
        }
    })
}

fn backend(config: &BackendConfig) -> Box<dyn Uploader> {
    match config {
        BackendConfig::Deferred => Box::new(DeferredUploader),
        BackendConfig::NixCopy { destination, nix } => {
            Box::new(NixCopyUploader::new(nix.clone(), destination.clone()))
        }
    }
}

fn scanner(config: &ScannerConfig) -> Box<dyn Scanner> {
    match config {
        ScannerConfig::NixPathInfo { nix } => Box::new(NixPathInfoScanner::new(nix.clone())),
        ScannerConfig::None => Box::new(NoScanner),
    }
}

fn uploader(path: &Path, once: bool, err: &mut dyn Write) -> u8 {
    let Some(text) = read_config(path, err) else {
        return EXIT_USAGE;
    };
    let config = match UploaderConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(err, "stockhouse uploader: {error}");
            return EXIT_USAGE;
        }
    };
    let queue = match Queue::create(&config.queue_dir, config.max_pending) {
        Ok(queue) => queue,
        Err(error) => {
            let _ = writeln!(err, "stockhouse uploader: {error}");
            return EXIT_FAILURE;
        }
    };
    let (backend, scanner) = (backend(&config.backend), scanner(&config.scanner));
    let mut worker = Worker::new(queue, backend.as_ref(), scanner.as_ref(), config.limits());
    loop {
        if let Err(error) = worker.pass(now(), err) {
            let _ = writeln!(err, "stockhouse uploader: {error}");
            if once {
                return EXIT_FAILURE;
            }
        }
        if once {
            return 0;
        }
        std::thread::sleep(Duration::from_secs(config.poll_interval_secs));
    }
}

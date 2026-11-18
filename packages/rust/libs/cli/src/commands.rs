use crate::EXIT_USAGE;
use crate::args::Parsed;
use std::io::Write;
use std::panic::{AssertUnwindSafe, UnwindSafe, catch_unwind};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use stockhouse_ingest::{
    BackendConfig, DeferredUploader, NixCopyUploader, NixPathInfoScanner, NoScanner, Queue,
    Scanner, ScannerConfig, Uploader, UploaderConfig, Worker, hook,
};
use stockhouse_proto::PROTOCOL_VERSION;
use stockhouse_proxy::ProxyConfig;
use stockhouse_upload::lease::{LeaseConfig, effective_uid, run_lease};
use stockhouse_upload::{ServerEndpoint, ServerUploader};

pub(crate) const EXIT_FAILURE: u8 = 1;

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
        Parsed::Login { config, device } => crate::session::login(&config, device, out, err),
        Parsed::Logout { config } => crate::session::logout(&config, out, err),
        Parsed::Refresh { config } => crate::session::refresh(&config, out, err),
        Parsed::SessionStatus { config } => crate::session::status(&config, out, err),
        Parsed::Status { queue } => status(&queue, out, err),
        Parsed::Hook { queue } => {
            let out_paths = env("OUT_PATHS").unwrap_or_default();
            guard_hook(AssertUnwindSafe(|| {
                hook::run(&queue, &out_paths, now(), err);
            }))
        }
        Parsed::Proxy { config } => proxy(&config, err),
        Parsed::Uploader { config, once } => uploader(&config, once, err),
        Parsed::Lease { config } => lease(&config, err),
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
                if error.is_configuration() {
                    EXIT_USAGE
                } else {
                    EXIT_FAILURE
                }
            }
        }
    })
}

/// Validates a `server` backend's endpoint before anything else is touched.
fn server_endpoint(config: &BackendConfig) -> Result<Option<ServerEndpoint>, String> {
    let BackendConfig::Server(server) = config else {
        return Ok(None);
    };
    ServerEndpoint::from_spec(
        &server.base_url,
        &server.credential,
        server.request_timeout_secs,
    )
    .map(Some)
    .map_err(|e| e.to_string())
}

fn backend(
    config: &BackendConfig,
    endpoint: Option<ServerEndpoint>,
    queue: &Queue,
) -> Box<dyn Uploader> {
    match (config, endpoint) {
        (BackendConfig::Deferred, _) => Box::new(DeferredUploader),
        (BackendConfig::NixCopy { destination, nix }, _) => {
            Box::new(NixCopyUploader::new(nix.clone(), destination.clone()))
        }
        (BackendConfig::Server(server), Some(endpoint)) => Box::new(ServerUploader::new(
            endpoint,
            server.nix.clone(),
            server.max_closure_paths,
            queue.clone(),
        )),
        (BackendConfig::Server(_), None) => {
            unreachable!("server_endpoint builds the endpoint of every server backend")
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
    let endpoint = match server_endpoint(&config.backend) {
        Ok(endpoint) => endpoint,
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
    let backend = backend(&config.backend, endpoint, &queue);
    let scanner = scanner(&config.scanner);
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

fn lease(path: &Path, err: &mut dyn Write) -> u8 {
    let Some(text) = read_config(path, err) else {
        return EXIT_USAGE;
    };
    let config = match LeaseConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => {
            let _ = writeln!(err, "stockhouse lease: {error}");
            return EXIT_USAGE;
        }
    };
    match run_lease(&config, effective_uid(), err) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(err, "stockhouse lease: {error}");
            EXIT_USAGE
        }
    }
}

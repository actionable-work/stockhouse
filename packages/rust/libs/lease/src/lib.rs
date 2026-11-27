//! `stockhouse lease`: renew this machine's named roots on the server.

mod batch;
mod config;
mod machine_id;
mod slots;

pub use config::{LeaseConfig, LeaseConfigError};
pub use machine_id::{MachineIdCheck, load_or_create};
pub use slots::{SlotPath, resolve_slots};

use reqwest::Method;
use reqwest::header::CONTENT_TYPE;
use std::io::Write;
use std::{sync::Arc, time::Duration};
use stockhouse_credentials::{CredentialSource, Secret};
use stockhouse_proto::StorePath;
use stockhouse_upload::{Exchange, Negotiator, ServerEndpoint};
use stockhouse_wire::{
    LeaseReply, LeaseRequest, LeaseSlot, MachineId, lease_path, parse_lease_reply,
};
use stockhouse_wire_v2::ProtocolMode;

/// Every slot was renewed or skipped.
pub const EXIT_OK: u8 = 0;
/// A slot was refused or failed, or no credential was available.
pub const EXIT_TEMPFAIL: u8 = 75;
/// The machine identity is unusable.
pub const EXIT_MACHINE_ID: u8 = 1;
const LEASE_REPLY_LIMIT: usize = 64 * 1024;

enum SlotResult {
    Renewed,
    NotRenewed,
}

fn renew_once(
    endpoint: &ServerEndpoint,
    id: &MachineId,
    slot: LeaseSlot,
    path: &StorePath,
    secret: &Secret,
) -> Option<Exchange> {
    let body = LeaseRequest {
        store_path: path.clone(),
    }
    .render();
    let request = endpoint
        .request(
            Method::PUT,
            &lease_path(id, slot),
            secret,
            endpoint.timeout(),
        )
        .ok()?
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    Some(endpoint.exchange(request, LEASE_REPLY_LIMIT))
}

fn classify(exchange: Option<Exchange>) -> Result<LeaseReply, &'static str> {
    let Some(exchange) = exchange else {
        return Err("credential unavailable: credential is not a valid header value");
    };
    match exchange {
        Exchange::Failed => Err("request failed"),
        Exchange::Reply(_, None) => Err("reply too large"),
        Exchange::Reply(status, Some(body)) => {
            parse_lease_reply(status, &body).map_err(|_| "unexpected reply")
        }
    }
}

fn renew(
    endpoint: &ServerEndpoint,
    id: &MachineId,
    slot: LeaseSlot,
    path: &StorePath,
    secret: &mut Secret,
    log: &mut dyn Write,
) -> SlotResult {
    let n = slot.get();
    let mut retried = false;
    loop {
        let reply = classify(renew_once(endpoint, id, slot, path, secret));
        let why = match reply {
            Ok(LeaseReply::Renewed) => {
                let _ = writeln!(log, "stockhouse lease: slot {n} renewed");
                return SlotResult::Renewed;
            }
            Ok(LeaseReply::Unauthorized) if !retried && endpoint.invalidate(secret) => {
                retried = true;
                match endpoint.credential() {
                    Ok(fresh) => {
                        *secret = fresh;
                        continue;
                    }
                    Err(_) => "credential unavailable",
                }
            }
            Ok(LeaseReply::Unauthorized) => "unauthorized",
            Ok(LeaseReply::Refused(refusal)) => refusal.code(),
            Ok(LeaseReply::Unavailable) => "unavailable",
            Err(why) => why,
        };
        let _ = writeln!(log, "stockhouse lease: slot {n} not renewed: {why}");
        return SlotResult::NotRenewed;
    }
}

/// The process's effective UID, the owner the machine identity must have.
pub fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Runs one lease renewal with the configured credential and returns the
/// exit status (0, 75 or 1); a credential source that cannot be built or is
/// for another private tier is a configuration error.
pub fn run_lease(
    config: &LeaseConfig,
    euid: u32,
    log: &mut dyn Write,
) -> Result<u8, LeaseConfigError> {
    let endpoint = ServerEndpoint::from_spec(
        &config.base_url,
        &config.credential,
        config.request_timeout_secs,
    )
    .map_err(LeaseConfigError::Endpoint)?;
    Ok(renew_all(config, &endpoint, euid, log, &std::thread::sleep))
}

/// Runs one lease renewal with an injected credential source.
pub fn run_lease_with(
    config: &LeaseConfig,
    source: Arc<dyn CredentialSource>,
    euid: u32,
    log: &mut dyn Write,
) -> Result<u8, LeaseConfigError> {
    run_lease_with_sleep(config, source, euid, log, &std::thread::sleep)
}

/// Runs a lease renewal with a credential source and wait function supplied by the caller.
#[doc(hidden)]
pub fn run_lease_with_sleep(
    config: &LeaseConfig,
    source: Arc<dyn CredentialSource>,
    euid: u32,
    log: &mut dyn Write,
    sleep: &dyn Fn(Duration),
) -> Result<u8, LeaseConfigError> {
    let endpoint = ServerEndpoint::new(&config.base_url, source, config.request_timeout_secs)
        .map_err(LeaseConfigError::Endpoint)?;
    Ok(renew_all(config, &endpoint, euid, log, sleep))
}

fn renew_all(
    config: &LeaseConfig,
    endpoint: &ServerEndpoint,
    euid: u32,
    log: &mut dyn Write,
    sleep: &dyn Fn(Duration),
) -> u8 {
    let id = match load_or_create(&config.machine_id_file, euid) {
        Ok(id) => id,
        Err(check) => {
            let _ = writeln!(log, "stockhouse lease: machine identity: {}", check.name());
            return EXIT_MACHINE_ID;
        }
    };
    let mut secret = match endpoint.credential() {
        Ok(secret) => secret,
        Err(error) => {
            let _ = writeln!(log, "stockhouse lease: credential unavailable: {error}");
            return EXIT_TEMPFAIL;
        }
    };
    let slots = resolved_slots(config, log);
    if slots.is_empty() {
        return EXIT_OK;
    }
    let negotiator = Negotiator::new(config.protocol);
    match negotiator.version_with(endpoint, 0, &mut secret) {
        Ok(1) => {}
        Ok(2) => match batch::renew_batch(endpoint, id, &slots, &mut secret, log, sleep) {
            batch::BatchStatus::Finished(status) => return status,
            batch::BatchStatus::NotSupported if config.protocol == ProtocolMode::Auto => {}
            batch::BatchStatus::NotSupported => {
                return batch::fail_slots(&slots, "protocol v2 not available", log);
            }
        },
        Ok(_) => unreachable!("the negotiator only selects implemented versions"),
        Err(error) => return batch::fail_slots(&slots, &error.to_string(), log),
    }
    let mut status = EXIT_OK;
    for (slot, path) in slots {
        if let SlotResult::NotRenewed = renew(endpoint, &id, slot, &path, &mut secret, log) {
            status = EXIT_TEMPFAIL;
        }
    }
    status
}

fn resolved_slots(config: &LeaseConfig, log: &mut dyn Write) -> Vec<(LeaseSlot, StorePath)> {
    resolve_slots(config)
        .into_iter()
        .filter_map(|slot| match slot {
            SlotPath::Unresolvable(slot) => {
                let _ = writeln!(
                    log,
                    "stockhouse lease: slot {} skipped: no store path",
                    slot.get()
                );
                None
            }
            SlotPath::Renew(slot, path) => Some((slot, path)),
        })
        .collect()
}

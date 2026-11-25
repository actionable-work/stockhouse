use crate::{EXIT_OK, EXIT_TEMPFAIL};
use std::{io::Write, time::Duration};
use stockhouse_credentials::Secret;
use stockhouse_proto::StorePath;
use stockhouse_upload::{BatchRequest, ServerEndpoint};
use stockhouse_wire::{LeaseSlot, MachineId};
use stockhouse_wire_v2::{
    BatchKind, BatchRefusal, BatchReply, EntryKey, EntryResult, LeaseBatch, LeaseEntry, LeaseResult,
};

pub(crate) enum BatchStatus {
    Finished(u8),
    NotSupported,
}

pub(crate) fn renew_batch(
    endpoint: &ServerEndpoint,
    id: MachineId,
    slots: &[(LeaseSlot, StorePath)],
    secret: &mut Secret,
    log: &mut dyn Write,
    sleep: &dyn Fn(Duration),
) -> BatchStatus {
    let mut slots = slots.to_vec();
    slots.sort_unstable_by_key(|(slot, _)| slot.get());
    let entries = slots
        .iter()
        .map(|(slot, path)| LeaseEntry {
            machine: id,
            slot: *slot,
            store_path: path.clone(),
        })
        .collect();
    let batch = LeaseBatch::new(entries).expect("resolved slots are unique, bounded and nonempty");
    let request = BatchRequest {
        kind: BatchKind::Leases,
        keys: slots
            .iter()
            .map(|(slot, _)| EntryKey::Lease(id, *slot))
            .collect(),
        body: batch.render(),
    };
    match request.send(endpoint, secret, sleep) {
        Ok(BatchReply::Results(results)) => {
            BatchStatus::Finished(log_results(&slots, results, log))
        }
        Ok(BatchReply::NotSupported) => BatchStatus::NotSupported,
        Ok(BatchReply::Refused(refusal)) => {
            let reason = match refusal {
                BatchRefusal::BadRequest => "bad_request",
                BatchRefusal::TooLarge => "protocol violation",
            };
            BatchStatus::Finished(fail_slots(&slots, reason, log))
        }
        Ok(_) => unreachable!("sender handles retries and unauthorized"),
        Err(error) => BatchStatus::Finished(fail_slots(&slots, &error.to_string(), log)),
    }
}

fn log_results(
    slots: &[(LeaseSlot, StorePath)],
    results: Vec<EntryResult>,
    log: &mut dyn Write,
) -> u8 {
    let mut status = EXIT_OK;
    for ((slot, _), result) in slots.iter().zip(results) {
        let why = match result {
            EntryResult::Lease(LeaseResult::Renewed) => {
                let _ = writeln!(log, "stockhouse lease: slot {} renewed", slot.get());
                continue;
            }
            EntryResult::Lease(LeaseResult::Refused(code)) => code.code(),
            EntryResult::Lease(LeaseResult::MachinesFull) => "machines_full",
            EntryResult::Upload(_) => unreachable!("lease reply only accepts lease entries"),
        };
        status = EXIT_TEMPFAIL;
        fail(*slot, why, log);
    }
    status
}

fn fail(slot: LeaseSlot, reason: &str, log: &mut dyn Write) {
    let _ = writeln!(
        log,
        "stockhouse lease: slot {} not renewed: {reason}",
        slot.get()
    );
}

pub(crate) fn fail_slots(
    slots: &[(LeaseSlot, StorePath)],
    reason: &str,
    log: &mut dyn Write,
) -> u8 {
    for (slot, _) in slots {
        fail(*slot, reason, log);
    }
    EXIT_TEMPFAIL
}

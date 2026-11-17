mod common;
use common::*;
use stockhouse_ingest::{
    NixPathInfoScanner, NoScanner, Queue, RescanError, ScanError, Scanner, Worker, parse_path_info,
    rescan,
};

fn fill(q: &Queue, labels: &[&str]) {
    let mut producer = q.producer().unwrap();
    for label in labels {
        producer.enqueue(&path(label), 1, true).unwrap();
    }
}

fn paths(labels: &[&str]) -> Vec<stockhouse_proto::StorePath> {
    labels.iter().map(|l| path(l)).collect()
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-001"))]
#[test]
fn overflow_triggers_a_rescan_when_capacity_is_free() {
    let q = queue(2);
    fill(&q, &["a", "b", "c"]);
    let uploader = FakeUploader::default();
    let scanner = FakeScanner(Ok(paths(&["c"])));
    let mut worker = Worker::new(q.clone(), &uploader, &scanner, limits());
    worker.pass(10, &mut Vec::new()).unwrap();
    let report = worker.pass(11, &mut Vec::new()).unwrap();
    assert!(
        report.rescanned,
        "overflow with free capacity rescans before the interval"
    );
    assert!(!q.status().unwrap().overflow.active);
    assert!(q.done_record(&path("c")).is_some());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-011"))]
#[test]
fn full_queue_skips_overflow_rescans() {
    let q = queue(1);
    fill(&q, &["a", "b"]);
    let scanner = FakeScanner(Ok(paths(&["b"])));
    let mut worker = Worker::new(
        q.clone(),
        &stockhouse_ingest::DeferredUploader,
        &scanner,
        limits(),
    );
    assert!(
        worker.pass(10, &mut Vec::new()).unwrap().rescanned,
        "first pass is the interval rescan"
    );
    let mut log = Vec::new();
    let report = worker.pass(11, &mut log).unwrap();
    assert!(!report.rescanned);
    assert!(q.status().unwrap().overflow.active);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-002"))]
#[test]
fn rescan_enqueues_missed_paths() {
    let q = queue(10);
    fill(&q, &["pending"]);
    let scanner = FakeScanner(Ok(paths(&["pending", "new1", "new2"])));
    let report = rescan(&q, &scanner, 5).unwrap();
    assert_eq!((report.enqueued, report.complete), (2, true));
    assert_eq!(q.status().unwrap().pending, 3);
    assert!(!q.root().join("overflow").exists());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-003"))]
#[test]
fn complete_rescan_clears_overflow() {
    let q = queue(1);
    fill(&q, &["a", "b"]);
    std::fs::remove_file(
        q.root()
            .join("pending")
            .join(format!("{}.json", path("a").hash())),
    )
    .unwrap();
    let report = rescan(&q, &FakeScanner(Ok(paths(&["b"]))), 5).unwrap();
    assert!(report.complete);
    assert!(!q.status().unwrap().overflow.active);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-004"))]
#[test]
fn rescan_at_capacity_keeps_overflow() {
    let q = queue(2);
    fill(&q, &["a", "b", "c"]);
    std::fs::remove_file(
        q.root()
            .join("pending")
            .join(format!("{}.json", path("a").hash())),
    )
    .unwrap();
    let report = rescan(&q, &FakeScanner(Ok(paths(&["c", "d", "e"]))), 5).unwrap();
    assert_eq!((report.enqueued, report.complete), (1, false));
    assert!(q.status().unwrap().overflow.active);
    let fresh = queue(1);
    fill(&fresh, &["x"]);
    assert!(!fresh.status().unwrap().overflow.active);
    rescan(&fresh, &FakeScanner(Ok(paths(&["y"]))), 5).unwrap();
    assert!(
        fresh.status().unwrap().overflow.active,
        "an interval rescan at capacity creates the marker"
    );
}

struct OverflowingScanner(Queue);

impl Scanner for OverflowingScanner {
    fn candidates(&self) -> Result<Vec<stockhouse_proto::StorePath>, ScanError> {
        self.0.producer().unwrap().overflow().unwrap();
        Ok(Vec::new())
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-005"))]
#[test]
fn overflow_during_rescan_stays_visible() {
    let q = queue(1);
    fill(&q, &["a", "b"]);
    rescan(&q, &OverflowingScanner(q.clone()), 5).unwrap();
    assert!(!q.root().join("overflow.rescan").exists());
    assert!(q.root().join("overflow").exists());
    assert!(q.status().unwrap().overflow.active);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-006"))]
#[test]
fn failed_scan_keeps_overflow() {
    let q = queue(1);
    fill(&q, &["a", "b"]);
    let result = rescan(&q, &FakeScanner(Err(ScanError::Format)), 5);
    assert!(matches!(result, Err(RescanError::Scan(ScanError::Format))));
    assert!(q.status().unwrap().overflow.active);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-010"))]
#[test]
fn none_scanner_never_clears_overflow() {
    let q = queue(1);
    fill(&q, &["a", "b"]);
    std::fs::remove_file(
        q.root()
            .join("pending")
            .join(format!("{}.json", path("a").hash())),
    )
    .unwrap();
    assert!(rescan(&q, &NoScanner, 5).unwrap().complete);
    assert!(q.status().unwrap().overflow.active);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-007"))]
#[test]
fn rescans_run_periodically() {
    let q = queue(10);
    let scanner = FakeScanner(Ok(Vec::new()));
    let mut worker = Worker::new(
        q.clone(),
        &stockhouse_ingest::DeferredUploader,
        &scanner,
        limits(),
    );
    let ran: Vec<bool> = [0, 500, 999, 1000, 1500, 2000]
        .iter()
        .map(|&t| worker.pass(t, &mut Vec::new()).unwrap().rescanned)
        .collect();
    assert_eq!(ran, [true, false, false, true, false, true]);
}

const FORMAT_1: &str = r#"{
 "/nix/store/pkf547jp8a92k4wmxikkyby58w10pysr-hello-1.0":{"ca":null,"deriver":null,"narHash":"sha256-x","narSize":1,"references":[],"registrationTime":1,"signatures":[],"ultimate":true},
 "/nix/store/49ma9vimj6wlsbrhqsra6g6fzvsj6shh-ncurses-6.6":{"narSize":1,"ultimate":false},
 "/nix/store/m07h00fl6538s4gavrp66a20ka4hg7fy-hello-1.0.drv":{"ultimate":true},
 "/nix/store/w5mnfv5d8w634ghw8x42z0n959gkbylv-noflag":{"narSize":1},
 "/nix/store/vqn9gw6ja6jwypif4hpcy9ygjnzzbg94-invalid":null
}"#;

const FORMAT_2: &str = r#"{"info":{
 "pkf547jp8a92k4wmxikkyby58w10pysr-hello-1.0":{"ca":null,"narSize":1,"storeDir":"/nix/store","ultimate":true,"version":2},
 "49ma9vimj6wlsbrhqsra6g6fzvsj6shh-ncurses-6.6":{"ultimate":false,"version":2}
},"storeDir":"/nix/store","version":2}"#;

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-008"))]
#[test]
fn nix_scanner_selects_locally_built_paths() {
    let hello = "/nix/store/pkf547jp8a92k4wmxikkyby58w10pysr-hello-1.0";
    for text in [FORMAT_1, FORMAT_2] {
        let found: Vec<String> = parse_path_info(text.as_bytes())
            .unwrap()
            .iter()
            .map(|p| p.to_string())
            .collect();
        assert_eq!(found, [hello]);
    }
    let nix = fake_nix(FORMAT_1.as_bytes(), b"warning: noise\n", 0);
    let found = NixPathInfoScanner::new(nix).candidates().unwrap();
    assert_eq!(
        found.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        [hello]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-009"))]
#[test]
fn unrecognized_scanner_output_is_an_error() {
    for text in [
        "[]",
        "[{\"path\":\"/nix/store/x\"}]",
        "{\"/tmp/x\":{}}",
        "{\"/nix/store/pkf547jp8a92k4wmxikkyby58w10pysr-a\":1}",
        "nope",
    ] {
        assert_eq!(
            parse_path_info(text.as_bytes()),
            Err(ScanError::Format),
            "{text}"
        );
    }
    let failing = fake_nix(b"", b"error", 1);
    assert!(matches!(
        NixPathInfoScanner::new(failing).candidates(),
        Err(ScanError::Command(_))
    ));
    let absent = NixPathInfoScanner::new(temp_dir().join("no-such-nix"));
    assert!(matches!(absent.candidates(), Err(ScanError::Command(_))));
    let dir = temp_dir();
    let big = dir.join("big");
    let script = format!("#!/bin/sh\nhead -c {} /dev/zero\n", 256 * 1024 * 1024 + 1);
    std::fs::write(&big, script).unwrap();
    std::fs::set_permissions(&big, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    while let Err(e) = std::process::Command::new(&big)
        .arg("-n")
        .stdout(std::process::Stdio::null())
        .status()
    {
        assert_eq!(e.raw_os_error(), Some(26), "{e}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        NixPathInfoScanner::new(big).candidates(),
        Err(ScanError::TooLarge)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-RESCAN-012"))]
#[test]
fn nix_scanner_passes_fixed_arguments() {
    let nix = fake_nix(b"{}", b"", 0);
    NixPathInfoScanner::new(nix.clone()).candidates().unwrap();
    assert_eq!(
        recorded_args(&nix),
        [
            "--extra-experimental-features",
            "nix-command",
            "path-info",
            "--json",
            "--all"
        ]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-QUEUE-016"))]
#[test]
fn a_corrupt_record_stops_a_rescan_without_new_overflow() {
    // Free capacity, so the enqueue reaches the corrupt record rather than
    // stopping at the capacity check.
    let q = queue(2);
    fill(&q, &["a", "b", "x"]);
    let pending = q.root().join("pending");
    for label in ["a", "b"] {
        std::fs::remove_file(pending.join(format!("{}.json", path(label).hash()))).unwrap();
    }
    std::fs::write(
        pending.join(format!("{}.json", path("c").hash())),
        b"{not json",
    )
    .unwrap();
    let report = rescan(&q, &FakeScanner(Ok(paths(&["c"]))), 5).unwrap();
    assert_eq!((report.enqueued, report.complete), (0, false));
    assert!(
        !q.root().join("overflow").exists(),
        "no live marker is written"
    );
    assert_eq!(
        std::fs::read(q.root().join("overflow.rescan")).unwrap(),
        b"o"
    );
    assert!(q.status().unwrap().overflow.active);
}

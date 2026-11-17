mod common;
use common::*;
use std::os::unix::fs::PermissionsExt;
use stockhouse_ingest::Queue;
use stockhouse_ingest::hook::{self, HookReport};

fn run(dir: &std::path::Path, out_paths: &str) -> (HookReport, Vec<String>) {
    let mut log = Vec::new();
    let report = hook::run(dir, out_paths, 100, &mut log);
    (report, String::from_utf8(log).unwrap().lines().map(str::to_owned).collect())
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-001"))]
#[test]
fn enqueues_every_built_output_within_limits() {
    let q = queue(2);
    let paths = format!("{} {} {}", path("a"), path("b"), path("c"));
    let (report, log) = run(q.root(), &paths);
    assert_eq!((report.queued, report.overflow), (2, 1));
    assert!(log.is_empty(), "{log:?}");
    assert_eq!(q.status().unwrap().pending, 2);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-002"))]
#[test]
fn invalid_paths_are_skipped() {
    let q = queue(10);
    let (report, log) = run(q.root(), &format!("/tmp/evil {} /nix/store/short-x", path("ok")));
    assert_eq!((report.queued, report.skipped), (1, 2));
    assert_eq!(log.len(), 2);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-003"))]
#[test]
fn unusable_queues_never_fail_the_build() {
    let base = temp_dir();
    let unsupported = Queue::create(&base.join("v2"), 5).unwrap();
    std::fs::write(unsupported.root().join("FORMAT"), "stockhouse-queue-v9\n").unwrap();
    let unwritable = Queue::create(&base.join("ro"), 5).unwrap();
    std::fs::set_permissions(unwritable.root().join("pending"), std::fs::Permissions::from_mode(0o500)).unwrap();
    std::fs::set_permissions(unwritable.root().join("tmp"), std::fs::Permissions::from_mode(0o500)).unwrap();
    std::fs::create_dir(base.join("bare")).unwrap();
    for dir in [base.join("missing"), base.join("bare"), base.join("v2"), base.join("ro")] {
        let (report, log) = run(&dir, path("a").as_str());
        assert_eq!(report.queued, 0, "{dir:?}");
        assert_eq!(log.len(), 1, "{dir:?}: {log:?}");
    }
    assert!(!base.join("missing").exists());
    assert_eq!(std::fs::read_dir(base.join("bare")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(unwritable.root().join("pending")).unwrap().count(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-005"))]
#[test]
fn hook_work_is_bounded() {
    let q = queue(5000);
    let paths: Vec<String> = (0..1030).map(|i| path(&format!("p{i}")).to_string()).collect();
    let (report, _) = run(q.root(), &paths.join(" "));
    assert_eq!((report.queued, report.overflow), (1024, 6));
    assert_eq!(q.status().unwrap().overflow.dropped, 6);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-006"))]
#[test]
fn hook_does_not_wait_for_uploads() {
    let q = queue(10);
    let started = std::time::Instant::now();
    let (report, _) = run(q.root(), path("a").as_str());
    assert_eq!(report.queued, 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let status = q.status().unwrap();
    assert_eq!((status.pending, status.uploaded), (1, 0));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-007"))]
#[test]
fn hook_never_creates_the_queue() {
    let dir = temp_dir().join("plain");
    std::fs::create_dir(&dir).unwrap();
    run(&dir, path("a").as_str());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-HOOK-008"))]
#[test]
fn symlinked_or_root_owned_queues_are_refused() {
    let q = queue(10);
    let link = temp_dir().join("link");
    std::os::unix::fs::symlink(q.root(), &link).unwrap();
    let (report, log) = run(&link, path("a").as_str());
    assert_eq!((report.queued, log.len()), (0, 1));
    assert_eq!(q.status().unwrap().pending, 0);
    let file = temp_dir().join("file");
    std::fs::write(&file, b"").unwrap();
    assert_eq!(run(&file, path("a").as_str()).1.len(), 1);
    assert_eq!(run(std::path::Path::new("/"), path("a").as_str()).1.len(), 1);
}

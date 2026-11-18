mod common;
use common::server::{error, state};
use common::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use stockhouse_credentials::{CredentialError, StaticCredential};
use stockhouse_upload::lease::{LeaseConfig, run_lease_with as run_lease};

/// A fake root with `/run/current-system`, system generations and a store.
struct Machine {
    root: PathBuf,
    state: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let dir = temp_dir();
        let root = std::fs::canonicalize(&dir).unwrap().join("root");
        for sub in ["run", "nix/store", "nix/var/nix/profiles"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        Self { root, state }
    }

    fn system(&self, label: &str) -> stockhouse_proto::StorePath {
        let path = store_path(label);
        std::fs::create_dir_all(self.on_disk(path.as_str())).unwrap();
        path
    }

    fn on_disk(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn link(&self, link: &str, target: &str) {
        let at = self.on_disk(link);
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(self.on_disk(target), at).unwrap();
    }

    fn config(&self, base: &str, extra: &str) -> LeaseConfig {
        let text = format!(
            "version = 1\nbase_url = \"{base}\"\nmachine_id_file = \"{}\"\nrequest_timeout_secs = 2\n\n[credential]\nsystemd = \"x\"\n{extra}",
            self.id_file().display()
        );
        LeaseConfig::parse(&text)
            .unwrap()
            .with_store_root(&self.root)
    }

    fn id_file(&self) -> PathBuf {
        self.state.join("machine-id")
    }
}

/// The effective UID, as the owner of a file this process creates.
fn euid() -> u32 {
    let probe = temp_dir().join("probe");
    std::fs::write(&probe, "").unwrap();
    std::fs::metadata(probe).unwrap().uid()
}

fn run(machine: &Machine, server: &FakeServer, extra: &str) -> (u8, String) {
    let mut log = Vec::new();
    let source = std::sync::Arc::new(StaticCredential::new(TOKEN.as_bytes()));
    let code = run_lease(
        &machine.config(&server.base(), extra),
        source,
        euid(),
        &mut log,
    )
    .unwrap();
    (code, String::from_utf8(log).unwrap())
}

fn leases(server: &FakeServer) -> Vec<(String, String)> {
    server
        .requests()
        .iter()
        .map(|r| {
            (
                r.rel().to_owned(),
                String::from_utf8(r.body.clone().unwrap_or_default()).unwrap(),
            )
        })
        .collect()
}

fn renewing() -> FakeServer {
    FakeServer::protocol(Default::default(), |_| None)
}

fn body(path: &stockhouse_proto::StorePath) -> String {
    format!("{{\"version\":1,\"store_path\":\"{path}\"}}")
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-003"))]
#[test]
fn the_machine_identity_is_created_once() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    let server = renewing();
    assert_eq!(run(&m, &server, "").0, 0);
    let meta = std::fs::metadata(m.id_file()).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let text = std::fs::read_to_string(m.id_file()).unwrap();
    assert_eq!(text.len(), 33);
    let id = stockhouse_wire::MachineId::parse(text.trim_end()).unwrap();
    assert_eq!(leases(&server)[0].0, format!("leases/{id}/1"));
    assert!(
        std::fs::read_dir(&m.state).unwrap().count() == 1,
        "no staging file left"
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-004"))]
#[test]
fn the_machine_identity_is_reused() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    let server = renewing();
    run(&m, &server, "");
    let (before, inode) = (
        std::fs::read(m.id_file()).unwrap(),
        std::fs::metadata(m.id_file()).unwrap().ino(),
    );
    run(&m, &server, "");
    assert_eq!(std::fs::read(m.id_file()).unwrap(), before);
    assert_eq!(std::fs::metadata(m.id_file()).unwrap().ino(), inode);
    let paths: Vec<String> = leases(&server).into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths[0], paths[1]);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-005"))]
#[test]
fn an_unusable_machine_identity_file_fails_closed() {
    let valid = "0123456789abcdef0123456789abcdef\n";
    let cases: [(&str, &str, u32, &str); 4] = [
        ("open", valid, 0o640, "grants group or other access"),
        (
            "upper",
            "0123456789ABCDEF0123456789ABCDEF\n",
            0o600,
            "malformed content",
        ),
        ("short", "0123\n", 0o600, "malformed content"),
        (
            "nonl",
            "0123456789abcdef0123456789abcdef",
            0o600,
            "malformed content",
        ),
    ];
    for (name, content, mode, check) in cases {
        let m = Machine::new();
        let current = m.system("system-a");
        m.link("/run/current-system", current.as_str());
        std::fs::write(m.id_file(), content).unwrap();
        std::fs::set_permissions(m.id_file(), std::fs::Permissions::from_mode(mode)).unwrap();
        let server = renewing();
        let (code, log) = run(&m, &server, "");
        assert_eq!(code, 1, "{name}");
        assert!(log.contains(check), "{name}: {log}");
        assert!(server.requests().is_empty(), "{name}");
        assert_eq!(
            std::fs::read_to_string(m.id_file()).unwrap(),
            content,
            "{name}"
        );
    }
    let m = Machine::new();
    let target = m.state.join("real");
    std::fs::write(&target, valid).unwrap();
    std::os::unix::fs::symlink(&target, m.id_file()).unwrap();
    let server = renewing();
    let (code, log) = run(&m, &server, "");
    assert_eq!(
        (code, log.contains("symbolic link refused")),
        (1, true),
        "{log}"
    );
    let mut wrong = Vec::new();
    let source = std::sync::Arc::new(StaticCredential::new(b"t"));
    std::fs::remove_file(m.id_file()).unwrap();
    std::fs::write(m.id_file(), valid).unwrap();
    std::fs::set_permissions(m.id_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = m.config(&server.base(), "");
    assert_eq!(
        run_lease(&config, source, euid().wrapping_add(1), &mut wrong).unwrap(),
        1
    );
    assert!(
        String::from_utf8(wrong)
            .unwrap()
            .contains("owned by another user")
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-006"))]
#[test]
fn slot_one_is_the_current_system() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    let server = renewing();
    run(&m, &server, "");
    let (path, sent) = leases(&server).remove(0);
    assert!(path.ends_with("/1"), "{path}");
    assert_eq!(sent, body(&current));
}

fn generations(
    m: &Machine,
) -> (
    stockhouse_proto::StorePath,
    stockhouse_proto::StorePath,
    stockhouse_proto::StorePath,
) {
    let (one, two, three) = (m.system("gen-1"), m.system("gen-2"), m.system("gen-3"));
    m.link("/nix/var/nix/profiles/system-1-link", one.as_str());
    m.link("/nix/var/nix/profiles/system-2-link", two.as_str());
    m.link("/nix/var/nix/profiles/system-10-link", three.as_str());
    m.link(
        "/nix/var/nix/profiles/system",
        "/nix/var/nix/profiles/system-10-link",
    );
    (one, two, three)
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-007"))]
#[test]
fn slot_two_is_the_newest_other_system_generation() {
    let m = Machine::new();
    let (_, two, three) = generations(&m);
    m.link("/run/current-system", three.as_str());
    let server = renewing();
    run(&m, &server, "");
    let sent = leases(&server);
    assert_eq!(sent.len(), 2);
    assert!(sent[1].0.ends_with("/2"));
    assert_eq!(sent[1].1, body(&two));
    // After a rollback to generation 2, slot 2 is generation 10.
    m.link("/run/current-system", two.as_str());
    let server = renewing();
    run(&m, &server, "");
    assert_eq!(leases(&server)[1].1, body(&three));
    // A single generation equal to the current system gives no slot 2.
    let m = Machine::new();
    let only = m.system("only");
    m.link("/nix/var/nix/profiles/system-1-link", only.as_str());
    m.link("/run/current-system", only.as_str());
    let server = renewing();
    assert_eq!(run(&m, &server, "").0, 0);
    assert_eq!(leases(&server).len(), 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-008"))]
#[test]
fn unresolvable_slots_are_skipped() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    std::fs::create_dir_all(m.on_disk("/etc/not-store")).unwrap();
    let extra = "[[extra]]\nslot = 3\npath = \"/missing\"\n[[extra]]\nslot = 4\npath = \"/etc/not-store\"\n[[extra]]\nslot = 5\npath = \"/run/current-system\"\n";
    let server = renewing();
    let (code, log) = run(&m, &server, extra);
    assert_eq!(code, 0);
    assert!(
        log.contains("slot 3 skipped") && log.contains("slot 4 skipped"),
        "{log}"
    );
    let paths: Vec<String> = leases(&server).into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths.len(), 2);
    assert!(paths[1].ends_with("/5"));
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-009"))]
#[test]
fn lease_renewals_use_the_protocol_path_and_body() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    let server = renewing();
    run(&m, &server, "");
    let req = server.requests().remove(0);
    let id = std::fs::read_to_string(m.id_file()).unwrap();
    assert_eq!(req.method, "PUT");
    assert_eq!(req.path, format!("/t/1/leases/{}/1", id.trim_end()));
    assert_eq!(req.header("content-type"), Some("application/json"));
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(req.body.unwrap(), body(&current).into_bytes());
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-010"))]
#[test]
fn a_failing_slot_does_not_stop_the_others() {
    let m = Machine::new();
    let (_, _, three) = generations(&m);
    m.link("/run/current-system", three.as_str());
    let extra = "[[extra]]\nslot = 3\npath = \"/run/current-system\"\n";
    let server = FakeServer::start(|req| {
        req.body.as_ref()?;
        Some(if req.path.ends_with("/1") {
            error(422, "unknown_path")
        } else if req.path.ends_with("/2") {
            Action::Reply(503, "machine table full".into())
        } else {
            state("renewed")
        })
    });
    let (code, log) = run(&m, &server, extra);
    assert_eq!(code, 75);
    assert_eq!(server.requests().len(), 3);
    assert!(log.contains("slot 1 not renewed: unknown_path"), "{log}");
    assert!(log.contains("slot 2 not renewed: unavailable"), "{log}");
    assert!(log.contains("slot 3 renewed"), "{log}");
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-011"))]
#[test]
fn lease_exit_status_reports_the_run() {
    let m = Machine::new();
    let current = m.system("system-a");
    m.link("/run/current-system", current.as_str());
    assert_eq!(run(&m, &renewing(), "").0, 0);
    let refused = FakeServer::start(|req| req.body.as_ref().map(|_| error(409, "conflict")));
    assert_eq!(run(&m, &refused, "").0, 75);
    let server = renewing();
    for err in [CredentialError::NotConfigured, CredentialError::Missing] {
        let (source, _) = counting(Some(1), err);
        let mut log = Vec::new();
        let code = run_lease(&m.config(&server.base(), ""), source, euid(), &mut log).unwrap();
        assert_eq!(code, 75);
    }
    assert!(server.requests().is_empty());
    std::fs::write(m.id_file(), "bad\n").unwrap();
    assert_eq!(run(&m, &renewing(), "").0, 1);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-LEASE-012"))]
#[test]
fn lease_runs_are_idempotent() {
    let m = Machine::new();
    let (_, _, three) = generations(&m);
    m.link("/run/current-system", three.as_str());
    let server = renewing();
    run(&m, &server, "");
    let listing = |dir: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = listing(&m.state);
    let first = leases(&server);
    run(&m, &server, "");
    let all = leases(&server);
    assert_eq!(&all[first.len()..], &first[..]);
    assert_eq!(listing(&m.state), before);
}

#[cfg_attr(any(), bloomery("CLIENT-INGEST-SERVER-007"))]
#[test]
fn a_lease_401_retries_once_only_after_a_successful_invalidate() {
    for (refreshes, code, requests) in [(true, 0, 2), (false, 75, 1)] {
        let m = Machine::new();
        let current = m.system("system-a");
        m.link("/run/current-system", current.as_str());
        let first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let f2 = first.clone();
        let server = FakeServer::start(move |req| {
            req.body.as_ref()?;
            Some(if f2.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Action::Reply(401, String::new())
            } else {
                state("renewed")
            })
        });
        let source = counting_source(None, CredentialError::Missing, refreshes);
        let mut log = Vec::new();
        let config = m.config(&server.base(), "");
        assert_eq!(
            run_lease(&config, source.clone(), euid(), &mut log).unwrap(),
            code
        );
        assert_eq!(server.requests().len(), requests, "{refreshes}");
        assert_eq!(
            source
                .invalidations
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

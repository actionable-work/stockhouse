//! `upload` mode: bearer and outage gate, declarations, NAR verification,
//! stall and lost-reply controls, leases and restart persistence.
mod common;

use common::{Reply, call, exchange, get, request, send_raw, start, temp_dir, wait_for};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use stockhouse_proto::{Sha256, nix32};

const TOKEN: &str = "fixture-bearer-7c1";
const AUTH: (&str, &str) = ("Authorization", "Bearer fixture-bearer-7c1");

struct Fake {
    addr: SocketAddr,
    root: PathBuf,
}

impl Fake {
    async fn start(root: &Path) -> Self {
        let token = root.join("token");
        std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
        let state = root.join("state");
        let addr = start(&[
            "upload",
            "--listen",
            "127.0.0.1:0",
            "--root",
            state.to_str().unwrap(),
            "--token-file",
            token.to_str().unwrap(),
        ])
        .await;
        // The control directory exists once the server accepts requests.
        get(addr, "/t/1/nix-cache-info").await;
        Self { addr, root: state }
    }

    async fn send(&self, method: &str, rel: &str, body: &[u8]) -> Option<Reply> {
        call(self.addr, method, &format!("/t/1/{rel}"), &[AUTH], body).await
    }

    async fn ok(&self, method: &str, rel: &str, body: &[u8]) -> Reply {
        self.send(method, rel, body).await.expect("a reply")
    }

    fn control(&self, name: &str, on: bool) {
        let file = self.root.join("control").join(name);
        if on {
            std::fs::write(file, "").unwrap();
        } else {
            std::fs::remove_file(file).unwrap();
        }
    }

    fn log(&self, name: &str) -> Vec<String> {
        let text = std::fs::read_to_string(self.root.join(name)).unwrap_or_default();
        text.lines().map(str::to_owned).collect()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.root.join("state.json")).unwrap()).unwrap()
    }
}

fn hash(tag: char) -> String {
    std::iter::repeat_n(tag, 32).collect()
}

fn base(tag: char) -> String {
    format!("{}-vm-{tag}", hash(tag))
}

fn decl(tag: char, refs: &[char], nar: &[u8]) -> Vec<u8> {
    let refs: Vec<String> = refs.iter().map(|r| base(*r)).collect();
    let nar_hash = format!("sha256:{}", nix32::encode(Sha256::digest(nar).bytes()));
    json!({
        "store_path": format!("/nix/store/{}", base(tag)),
        "references": refs,
        "nar_hash": nar_hash,
        "nar_size": nar.len(),
    })
    .to_string()
    .into_bytes()
}

fn status_of(reply: &Reply) -> (u16, Value) {
    (reply.status, reply.json())
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-007"))]
#[tokio::test(flavor = "multi_thread")]
async fn the_bearer_and_outage_gate() {
    let fake = Fake::start(&temp_dir()).await;
    let info = get(fake.addr, "/t/1/nix-cache-info").await;
    assert_eq!(
        (info.status, info.body.as_str()),
        (200, "StoreDir: /nix/store\n")
    );
    assert_eq!(
        get(fake.addr, &format!("/t/1/{}.narinfo", hash('a')))
            .await
            .status,
        404
    );
    let missing = get(fake.addr, "/t/1/uploads/u0").await;
    assert_eq!((missing.status, missing.body.as_str()), (401, ""));
    assert!(
        missing.head.contains("connection: close"),
        "{}",
        missing.head
    );
    let wrong = call(
        fake.addr,
        "GET",
        "/t/1/uploads/u0",
        &[("Authorization", "Bearer other")],
        b"",
    )
    .await;
    assert_eq!(wrong.unwrap().status, 401);
    let unknown = fake.ok("GET", "uploads/u0", b"").await;
    assert_eq!(
        status_of(&unknown),
        (404, json!({"error": "unknown_upload"}))
    );
    fake.control("down", true);
    let down = fake.ok("POST", "uploads/u0", &decl('a', &[], b"n")).await;
    assert_eq!(status_of(&down), (503, json!({"error": "unavailable"})));
    assert_eq!(
        get(fake.addr, "/t/1/nix-cache-info").await.status,
        200,
        "open during outage"
    );
    assert_eq!(
        fake.log("requests.log"),
        [
            "GET uploads/u0 - 401",
            "GET uploads/u0 - 401",
            "GET uploads/u0 - unknown_upload",
            "POST uploads/u0 - 503"
        ]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-008"))]
#[tokio::test(flavor = "multi_thread")]
async fn declaration_outcomes() {
    let fake = Fake::start(&temp_dir()).await;
    let bad = fake.ok("POST", "uploads/u0", b"{\"store_path\": 3}").await;
    assert_eq!(status_of(&bad), (400, json!({"error": "bad_request"})));
    let missing = fake
        .ok("POST", "uploads/ub", &decl('b', &['b', 'a', 'c'], b"nb"))
        .await;
    let expected = json!({"error": "missing_references", "missing": [base('a'), base('c')]});
    assert_eq!(status_of(&missing), (422, expected));
    fake.control(&format!("present-{}", hash('a')), true);
    fake.control(&format!("present-{}", hash('c')), true);
    assert_eq!(
        status_of(
            &fake
                .ok("POST", "uploads/ub", &decl('b', &['a', 'c'], b"nb"))
                .await
        ),
        (200, json!({"state": "open"}))
    );
    assert_eq!(
        fake.ok("POST", "uploads/ub", &decl('b', &[], b"nb"))
            .await
            .json(),
        json!({"state": "open"})
    );
    let present = fake.ok("POST", "uploads/ua", &decl('a', &[], b"na")).await;
    assert_eq!(status_of(&present), (200, json!({"state": "present"})));
    fake.control("quota", true);
    let quota = fake.ok("POST", "uploads/ud", &decl('d', &[], b"nd")).await;
    assert_eq!(status_of(&quota), (507, json!({"error": "quota_exceeded"})));
    fake.control("quota", false);
    let state = fake.state();
    assert_eq!(state["uploads"]["ub"]["state"], "open");
    assert_eq!(
        state["uploads"]["ub"]["decl"]["references"],
        json!([base('a'), base('c')])
    );
    assert!(state["uploads"].get("ua").is_none() && state["uploads"].get("ud").is_none());
    let (b, a, d) = (hash('b'), hash('a'), hash('d'));
    assert_eq!(
        fake.log("requests.log"),
        [
            "POST uploads/u0 - bad_request".to_owned(),
            format!("POST uploads/ub {b} missing_references"),
            format!("POST uploads/ub {b} open"),
            format!("POST uploads/ub {b} open"),
            format!("POST uploads/ua {a} present"),
            format!("POST uploads/ud {d} quota_exceeded"),
        ]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-009"))]
#[tokio::test(flavor = "multi_thread")]
async fn nar_bodies_are_verified_before_publishing() {
    let fake = Fake::start(&temp_dir()).await;
    let unknown = fake.ok("PUT", "uploads/nope/nar", b"x").await;
    assert_eq!(
        status_of(&unknown),
        (404, json!({"error": "unknown_upload"}))
    );
    fake.ok("POST", "uploads/ue", &decl('e', &[], b"the nar"))
        .await;
    for body in [&b"the nar!"[..], b"the naR"] {
        let refused = fake.ok("PUT", "uploads/ue/nar", body).await;
        assert_eq!(
            status_of(&refused),
            (400, json!({"error": "verification_failed"}))
        );
    }
    fake.control(&format!("reject-{}", hash('e')), true);
    let rejected = fake.ok("PUT", "uploads/ue/nar", b"the nar").await;
    assert_eq!(rejected.json(), json!({"error": "verification_failed"}));
    fake.control(&format!("reject-{}", hash('e')), false);
    let published = fake.ok("PUT", "uploads/ue/nar", b"the nar").await;
    assert_eq!(status_of(&published), (200, json!({"state": "published"})));
    assert_eq!(
        fake.ok("GET", "uploads/ue", b"").await.json(),
        json!({"state": "published"})
    );
    assert_eq!(
        fake.ok("POST", "uploads/ue", &decl('e', &[], b"the nar"))
            .await
            .json(),
        json!({"state": "published"})
    );
    assert_eq!(fake.state()["held"], json!([base('e')]));
    let e = hash('e');
    assert_eq!(
        fake.log("requests.log")[2..],
        [
            format!("PUT uploads/ue/nar {e} verification_failed"),
            format!("PUT uploads/ue/nar {e} verification_failed"),
            format!("PUT uploads/ue/nar {e} verification_failed"),
            format!("PUT uploads/ue/nar {e} published"),
            format!("GET uploads/ue {e} published"),
            format!("POST uploads/ue {e} published"),
        ]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-010"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_nar_waits_for_release() {
    let fake = Fake::start(&temp_dir()).await;
    fake.ok("POST", "uploads/us", &decl('s', &[], b"stalled nar"))
        .await;
    let stall = format!("stall-{}", hash('s'));
    fake.control(&stall, true);
    let raw = request("PUT", "/t/1/uploads/us/nar", &[AUTH], b"stalled nar");
    let addr = fake.addr;
    let pending = tokio::spawn(async move { exchange(addr, &raw).await });
    wait_for(&fake.root.join("requests.log"), " stalled").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!pending.is_finished(), "a stalled NAR is not answered");
    fake.control(&stall, false);
    let reply = pending.await.unwrap().expect("a reply after release");
    assert_eq!(status_of(&reply), (200, json!({"state": "published"})));
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-010"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_reply_publishes_and_hangs_up_once() {
    let fake = Fake::start(&temp_dir()).await;
    fake.ok("POST", "uploads/ux", &decl('x', &[], b"dropped nar"))
        .await;
    let lost = format!("drop-{}", hash('x'));
    fake.control(&lost, true);
    assert!(
        fake.send("PUT", "uploads/ux/nar", b"dropped nar")
            .await
            .is_none()
    );
    assert!(
        !fake.root.join("control").join(&lost).exists(),
        "the drop file is consumed"
    );
    assert_eq!(
        fake.ok("GET", "uploads/ux", b"").await.json(),
        json!({"state": "published"})
    );
    let short = b"PUT /t/1/uploads/ux/nar HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-bearer-7c1\r\nContent-Length: 100\r\n\r\npartial";
    assert!(send_raw(fake.addr, short, true).await.is_none());
    let x = hash('x');
    assert_eq!(
        fake.log("requests.log")[1..],
        [
            format!("PUT uploads/ux/nar {x} dropped"),
            format!("GET uploads/ux {x} published"),
            format!("PUT uploads/ux/nar {x} aborted"),
        ]
    );
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-012"))]
#[tokio::test(flavor = "multi_thread")]
async fn lease_renewals_are_recorded() {
    let fake = Fake::start(&temp_dir()).await;
    let path = format!("/nix/store/{}", base('l'));
    let body = json!({ "store_path": path }).to_string();
    let renewed = fake.ok("PUT", "leases/m1/2", body.as_bytes()).await;
    assert_eq!(status_of(&renewed), (200, json!({"state": "renewed"})));
    assert_eq!(fake.log("leases.log"), [format!("m1 2 {path}")]);
    assert_eq!(fake.log("requests.log"), ["PUT leases/m1/2 - renewed"]);
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-011"))]
#[tokio::test(flavor = "multi_thread")]
async fn state_survives_a_restart() {
    let dir = temp_dir();
    let first = Fake::start(&dir).await;
    first
        .ok("POST", "uploads/ur", &decl('r', &[], b"kept"))
        .await;
    first.ok("PUT", "uploads/ur/nar", b"kept").await;
    let body = json!({ "store_path": "/nix/store/x" }).to_string();
    first.ok("PUT", "leases/m/1", body.as_bytes()).await;
    let second = Fake::start(&dir).await;
    assert_ne!(first.addr, second.addr);
    assert_eq!(
        second.ok("GET", "uploads/ur", b"").await.json(),
        json!({"state": "published"})
    );
    let again = second
        .ok("POST", "uploads/other", &decl('r', &[], b"kept"))
        .await;
    assert_eq!(again.json(), json!({"state": "present"}));
    assert_eq!(second.log("requests.log").len(), 5);
    assert_eq!(second.log("leases.log"), ["m 1 /nix/store/x"]);
}

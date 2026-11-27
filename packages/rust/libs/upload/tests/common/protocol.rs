//! The in-memory protocol v1 dispatcher used by the upload tests.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use stockhouse_proto::Sha256;
use stockhouse_test_support::server::{Action, FakeServer, Request, error, state as state_reply};
use stockhouse_wire::UploadDeclaration;

#[derive(Default)]
struct State {
    uploads: HashMap<String, (UploadDeclaration, &'static str)>,
    held: HashSet<String>,
}

/// A minimal protocol v1 server over in-memory state.
#[derive(Default)]
pub struct Protocol {
    state: Mutex<State>,
}

impl Protocol {
    /// A protocol server, with `before` consulted first on full requests.
    pub fn start(
        proto: Arc<Protocol>,
        before: impl Fn(&Request) -> Option<Action> + Send + Sync + 'static,
    ) -> FakeServer {
        FakeServer::start(move |req| {
            req.body.as_ref()?;
            before(req).or_else(|| Some(proto.handle(req)))
        })
    }

    /// Marks a store path basename as already held.
    pub fn hold(&self, basename: &str) {
        self.state.lock().unwrap().held.insert(basename.to_owned());
    }

    /// Forgets a held path (as server GC would).
    pub fn forget(&self, basename: &str) {
        self.state.lock().unwrap().held.remove(basename);
    }

    pub fn holds(&self, basename: &str) -> bool {
        self.state.lock().unwrap().held.contains(basename)
    }

    pub fn handle(&self, req: &Request) -> Action {
        let bearer = format!("Bearer {}", super::TOKEN);
        if req.header("authorization") != Some(bearer.as_str()) {
            return Action::Reply(401, String::new());
        }
        let rel = req.rel().to_owned();
        let parts: Vec<&str> = rel.split('/').collect();
        let mut state = self.state.lock().unwrap();
        match (req.method.as_str(), parts.as_slice()) {
            ("POST", ["uploads", id]) => declare(&mut state, id, req),
            ("GET", ["uploads", id]) => match state.uploads.get(*id) {
                Some((_, s)) => state_reply(s),
                None => error(404, "unknown_upload"),
            },
            ("PUT", ["uploads", id, "nar"]) => nar(&mut state, id, req),
            ("PUT", ["leases", _, _]) => state_reply("renewed"),
            _ => Action::Reply(404, String::new()),
        }
    }
}

fn declare(state: &mut State, id: &str, req: &Request) -> Action {
    let Ok(decl) = UploadDeclaration::parse(req.body.as_deref().unwrap_or_default()) else {
        return error(400, "bad_request");
    };
    let base = decl.store_path().basename().to_owned();
    if let Some((_, "published")) = state.uploads.get(id) {
        return state_reply("published");
    }
    if state.held.contains(&base) {
        return state_reply("present");
    }
    let missing: Vec<String> = decl
        .references()
        .iter()
        .map(|r| r.basename().to_owned())
        .filter(|r| *r != base && !state.held.contains(r))
        .map(|r| format!("\"{r}\""))
        .collect();
    if !missing.is_empty() {
        let body = format!(
            "{{\"error\":\"missing_references\",\"missing\":[{}]}}",
            missing.join(",")
        );
        return Action::Reply(422, body);
    }
    state.uploads.entry(id.to_owned()).or_insert((decl, "open"));
    state_reply("open")
}

fn nar(state: &mut State, id: &str, req: &Request) -> Action {
    let Some((decl, _)) = state.uploads.get(id).cloned() else {
        return error(404, "unknown_upload");
    };
    if !req.complete {
        return Action::Close;
    }
    let body = req.body.as_deref().unwrap_or_default();
    if Sha256::digest(body) != decl.nar_hash() || body.len() as u64 != decl.nar_size() {
        return error(400, "verification_failed");
    }
    state.held.insert(decl.store_path().basename().to_owned());
    state.uploads.insert(id.to_owned(), (decl, "published"));
    state_reply("published")
}

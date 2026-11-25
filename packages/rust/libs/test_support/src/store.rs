//! Fake local store and a script that emulates the Nix calls under test.
use crate::{store_path, temp_dir};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use stockhouse_ingest::{EnqueueOutcome, Queue, UploadJob};
use stockhouse_proto::{Sha256, StorePath};

const SCRIPT: &str = r#"#!/bin/sh
d="@DIR@"
printf '%s\n' "$*" >> "$d/args"
case "$3" in
path-info)
  b=${6##*/}; h=${b%%-*}
  if [ -f "$d/closure.code" ]; then exit "$(cat "$d/closure.code")"; fi
  if [ -f "$d/closure.sleep" ]; then echo $$ > "$d/closure.pid"; exec sleep 30; fi
  cat "$d/closure/$h.json" ;;
store)
  b=${5##*/}; h=${b%%-*}
  if [ -f "$d/dump.sleep" ]; then echo $$ > "$d/dump.pid"; exec sleep 30; fi
  cat "$d/nar/$h"
  if [ -f "$d/dump.code" ]; then exit "$(cat "$d/dump.code")"; fi ;;
esac
"#;

struct Entry {
    path: StorePath,
    nar: Vec<u8>,
    refs: Vec<StorePath>,
}

/// A fake store root, NAR fixtures and a fake `nix`.
pub struct Store {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub nix: PathBuf,
    entries: Vec<Entry>,
}

impl Store {
    pub fn new() -> Self {
        let dir = temp_dir();
        let root = dir.join("root");
        for sub in ["closure", "nar"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let nix = dir.join("nix");
        std::fs::write(&nix, SCRIPT.replace("@DIR@", &dir.display().to_string())).unwrap();
        std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A parallel test may fork while the script is open for writing (ETXTBSY).
        while let Err(e) = std::process::Command::new(&nix).output() {
            assert_eq!(e.raw_os_error(), Some(26), "{e}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(dir.join("args"));
        Self {
            root,
            dir,
            nix,
            entries: Vec::new(),
        }
    }

    /// Adds a path with a NAR of `size` bytes and the given references.
    pub fn add_sized(&mut self, label: &str, refs: &[&StorePath], size: usize) -> StorePath {
        let path = store_path(label);
        let nar: Vec<u8> = (0..size)
            .map(|i| (i % 251) as u8 ^ label.len() as u8)
            .collect();
        std::fs::create_dir_all(self.on_disk(&path)).unwrap();
        std::fs::write(self.dir.join("nar").join(path.hash().as_str()), &nar).unwrap();
        self.entries.push(Entry {
            path: path.clone(),
            nar,
            refs: refs.iter().map(|r| (*r).clone()).collect(),
        });
        self.write_closure(&path);
        path
    }

    pub fn add(&mut self, label: &str, refs: &[&StorePath]) -> StorePath {
        self.add_sized(label, refs, 1000 + label.len())
    }

    pub fn on_disk(&self, path: &StorePath) -> PathBuf {
        self.root.join(path.as_str().trim_start_matches('/'))
    }

    pub fn nar(&self, path: &StorePath) -> &[u8] {
        &self.entry(path).nar
    }

    fn entry(&self, path: &StorePath) -> &Entry {
        self.entries.iter().find(|e| &e.path == path).unwrap()
    }

    fn closure_of(&self, path: &StorePath) -> Vec<&Entry> {
        let mut out: Vec<&Entry> = Vec::new();
        let mut todo = vec![path.clone()];
        while let Some(p) = todo.pop() {
            if out.iter().any(|e| e.path == p) {
                continue;
            }
            let entry = self.entry(&p);
            todo.extend(entry.refs.iter().filter(|r| **r != p).cloned());
            out.push(entry);
        }
        out
    }

    /// The plain `nix path-info --json --recursive` output for a path.
    pub fn closure_json(&self, path: &StorePath) -> String {
        let items: Vec<String> = self
            .closure_of(path)
            .iter()
            .map(|e| {
                let refs: Vec<String> = e.refs.iter().map(|r| format!("\"{r}\"")).collect();
                format!(
                    "\"{}\":{{\"narHash\":\"{}\",\"narSize\":{},\"references\":[{}],\"deriver\":null,\"ca\":null,\"ultimate\":true,\"registrationTime\":1}}",
                    e.path,
                    Sha256::digest(&e.nar),
                    e.nar.len(),
                    refs.join(",")
                )
            })
            .collect();
        format!("{{{}}}", items.join(","))
    }

    fn write_closure(&self, path: &StorePath) {
        let file = self
            .dir
            .join("closure")
            .join(format!("{}.json", path.hash()));
        std::fs::write(file, self.closure_json(path)).unwrap();
    }

    pub fn set(&self, name: &str, value: &str) {
        std::fs::write(self.dir.join(name), value).unwrap();
    }

    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn queue(&self) -> Queue {
        Queue::create(&self.dir.join("queue"), 64)
            .unwrap()
            .with_store_root(&self.root)
    }
}

/// Enqueues `path` and returns its job.
pub fn job(queue: &Queue, path: &StorePath) -> UploadJob {
    let id = match queue.producer().unwrap().enqueue(path, 0, false).unwrap() {
        EnqueueOutcome::Queued(id) | EnqueueOutcome::AlreadyQueued(id) => id,
        other => panic!("{other:?}"),
    };
    UploadJob {
        id,
        store_path: path.clone(),
        attempts: 0,
    }
}

pub fn wait_gone(pid_file: &Path) {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    for _ in 0..100 {
        if !proc.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("dump process {pid} still running");
}

pub fn entry_file(queue: &Queue, path: &StorePath) -> PathBuf {
    queue.root().join(format!("uploads/{}.json", path.hash()))
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

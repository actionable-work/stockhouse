//! Token store component tests. Keys are synthetic test constants.

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use stockhouse_credentials::{Secret, SystemdCredential};
use stockhouse_token_store::{
    Binding, FILE_NAME, MAGIC, SEALED_NAME, Store, StoreError, StoreMode, TokenSet,
};

const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const OTHER_KEY: &str = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100";
const ACCESS: &str = "synthetic-access-7c1e";
const REFRESH: &str = "synthetic-refresh-93ab";

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-store-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("store")).unwrap();
    fs::create_dir_all(dir.join("creds")).unwrap();
    fs::set_permissions(dir.join("store"), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn binding() -> Binding {
    Binding::new(
        "https://example.invalid/oauth/token",
        "stockhouse-cli",
        "https://cache.example.invalid/t/1/",
    )
}

fn key_source(root: &Path, key: &str) -> SystemdCredential {
    fs::write(root.join("creds/key"), format!("{key}\n")).unwrap();
    SystemdCredential::with_directory("key".to_owned(), Some(root.join("creds")))
}

fn sealed(root: &Path) -> Store {
    Store::new(
        root.join("store"),
        StoreMode::Sealed(key_source(root, KEY)),
        binding(),
    )
}

fn plain(root: &Path) -> Store {
    Store::new(root.join("store"), StoreMode::File, binding())
}

fn set() -> TokenSet {
    TokenSet {
        access_token: Secret::new(ACCESS.as_bytes().to_vec()),
        expires_at: 1_700_000_600,
        refresh_token: Some(Secret::new(REFRESH.as_bytes().to_vec())),
        scope: Some("cache:read".to_owned()),
        obtained_at: 1_700_000_000,
    }
}

fn write(store: &Store, set: &TokenSet) -> Result<(), StoreError> {
    let lock = store.lock(Duration::from_secs(1))?;
    store.write(&lock, set)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn assert_same(read: &TokenSet, expected: &TokenSet) {
    assert_eq!(read.access_token.expose(), expected.access_token.expose());
    assert_eq!(
        read.refresh_token.as_ref().map(|s| s.expose().to_vec()),
        expected.refresh_token.as_ref().map(|s| s.expose().to_vec())
    );
    assert_eq!(
        (read.expires_at, read.obtained_at, &read.scope),
        (expected.expires_at, expected.obtained_at, &expected.scope)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-001"))]
#[test]
fn sealed_set_round_trips_without_plaintext() {
    let root = temp_dir();
    let store = sealed(&root);
    write(&store, &set()).unwrap();
    assert_same(&store.read().unwrap().unwrap(), &set());
    let bytes = fs::read(root.join("store").join(SEALED_NAME)).unwrap();
    assert!(!contains(&bytes, ACCESS) && !contains(&bytes, REFRESH));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-002"))]
#[test]
fn sealing_uses_a_fresh_nonce() {
    let root = temp_dir();
    let store = sealed(&root);
    let path = root.join("store").join(SEALED_NAME);
    write(&store, &set()).unwrap();
    let first = fs::read(&path).unwrap();
    write(&store, &set()).unwrap();
    let second = fs::read(&path).unwrap();
    assert_eq!(&first[..8], MAGIC);
    assert_eq!(&first[8..40], binding().bytes());
    assert_eq!(first[..40], second[..40]);
    assert_ne!(first[40..64], second[40..64], "nonce differs");
    assert_ne!(first[64..], second[64..], "ciphertext differs");
    // Independent check: the format decrypts with the header as AAD.
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let key: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&KEY[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let cipher = chacha20poly1305::XChaCha20Poly1305::new_from_slice(&key).unwrap();
    let nonce: [u8; 24] = first[40..64].try_into().unwrap();
    let payload = Payload {
        msg: &first[64..],
        aad: &first[..40],
    };
    let plain = cipher.decrypt(&nonce.into(), payload).unwrap();
    assert!(contains(&plain, ACCESS));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-003"))]
#[test]
fn tampered_or_wrongly_keyed_stores_are_refused() {
    let root = temp_dir();
    let store = sealed(&root);
    let path = root.join("store").join(SEALED_NAME);
    write(&store, &set()).unwrap();
    let good = fs::read(&path).unwrap();
    let damaged: Vec<Vec<u8>> = vec![
        good[..70].to_vec(),
        {
            let mut b = good.clone();
            let last = b.len() - 1;
            b[last] ^= 1;
            b
        },
        {
            let mut b = good.clone();
            b[0] = b'X';
            b
        },
    ];
    for bytes in damaged {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.read().map(|_| ()), Err(StoreError::Corrupt));
        assert_eq!(fs::read(&path).unwrap(), bytes, "file unchanged");
    }
    fs::write(&path, &good).unwrap();
    let other = Store::new(
        root.join("store"),
        StoreMode::Sealed(key_source(&root, OTHER_KEY)),
        binding(),
    );
    assert_eq!(other.read().map(|_| ()), Err(StoreError::Corrupt));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-004"))]
#[test]
fn store_bound_to_another_configuration_is_refused() {
    let root = temp_dir();
    write(&sealed(&root), &set()).unwrap();
    write(&plain(&root), &set()).unwrap();
    let other = Binding::new(
        "https://example.invalid/oauth/token",
        "another-client",
        "https://cache.example.invalid/t/1/",
    );
    let file = Store::new(root.join("store"), StoreMode::File, other);
    assert_eq!(file.read().map(|_| ()), Err(StoreError::Mismatch));
    write(&sealed(&root), &set()).unwrap();
    let sealed_other = Store::new(
        root.join("store"),
        StoreMode::Sealed(key_source(&root, KEY)),
        other,
    );
    assert_eq!(sealed_other.read().map(|_| ()), Err(StoreError::Mismatch));
    assert!(root.join("store").join(SEALED_NAME).exists());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-005"))]
#[test]
fn sealing_key_comes_from_a_systemd_credential() {
    let root = temp_dir();
    let unset = SystemdCredential::with_directory("key".to_owned(), None);
    let missing = SystemdCredential::with_directory("key".to_owned(), Some(root.join("creds")));
    for source in [unset, missing] {
        let store = Store::new(root.join("store"), StoreMode::Sealed(source), binding());
        assert_eq!(store.read().map(|_| ()), Err(StoreError::KeyUnavailable));
    }
    let upper = KEY.to_uppercase();
    let bad = [
        &KEY[..63],
        &format!("{KEY}0")[..],
        &upper[..],
        &"g".repeat(64)[..],
        "",
    ];
    for key in bad {
        let source = key_source(&root, key);
        let store = Store::new(root.join("store"), StoreMode::Sealed(source), binding());
        write(&plain(&root), &set()).unwrap();
        fs::write(root.join("store").join(SEALED_NAME), b"x").unwrap();
        assert_eq!(
            store.read().map(|_| ()),
            Err(StoreError::KeyMalformed),
            "{key}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-006"))]
#[test]
fn plaintext_storage_only_when_configured() {
    let root = temp_dir();
    write(&sealed(&root), &set()).unwrap();
    assert!(!root.join("store").join(FILE_NAME).exists());
    write(&plain(&root), &set()).unwrap();
    let path = root.join("store").join(FILE_NAME);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert!(contains(&fs::read(&path).unwrap(), ACCESS));
    assert_same(&plain(&root).read().unwrap().unwrap(), &set());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-007"))]
#[test]
fn writes_are_atomic() {
    let root = temp_dir();
    let store = sealed(&root);
    let dir = root.join("store");
    write(&store, &set()).unwrap();
    let before = fs::read(dir.join(SEALED_NAME)).unwrap();
    let inode = fs::metadata(dir.join(SEALED_NAME)).unwrap().ino();
    write(&store, &set()).unwrap();
    assert_ne!(fs::metadata(dir.join(SEALED_NAME)).unwrap().ino(), inode);
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        let lock = store.lock(Duration::from_secs(1)).unwrap();
        let current = fs::read(dir.join(SEALED_NAME)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            store.write(&lock, &set()),
            Err(StoreError::Io { op: "create" })
        );
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read(dir.join(SEALED_NAME)).unwrap(), current);
        assert_ne!(current, before);
    }
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-007"))]
#[test]
fn failed_rename_leaves_the_previous_store() {
    let root = temp_dir();
    let dir = root.join("store");
    // A previous plaintext store, and a directory squatting on the sealed name.
    write(&plain(&root), &set()).unwrap();
    let previous = fs::read(dir.join(FILE_NAME)).unwrap();
    fs::create_dir(dir.join(SEALED_NAME)).unwrap();
    fs::write(dir.join(SEALED_NAME).join("keep"), b"squatter").unwrap();
    let store = sealed(&root);
    let lock = store.lock(Duration::from_secs(1)).unwrap();
    assert_eq!(
        store.write(&lock, &set()),
        Err(StoreError::Io { op: "rename" })
    );
    assert_eq!(fs::read(dir.join(FILE_NAME)).unwrap(), previous);
    assert_eq!(
        fs::read(dir.join(SEALED_NAME).join("keep")).unwrap(),
        b"squatter"
    );
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-008"))]
#[test]
fn switching_modes_leaves_no_stale_copy() {
    let root = temp_dir();
    let dir = root.join("store");
    write(&plain(&root), &set()).unwrap();
    write(&sealed(&root), &set()).unwrap();
    assert!(!dir.join(FILE_NAME).exists() && dir.join(SEALED_NAME).exists());
    write(&plain(&root), &set()).unwrap();
    assert!(dir.join(FILE_NAME).exists() && !dir.join(SEALED_NAME).exists());
}

fn mkfifo(path: &Path) {
    let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-009"))]
#[test]
fn unsafe_store_files_are_refused() {
    let root = temp_dir();
    let store = plain(&root);
    let path = root.join("store").join(FILE_NAME);
    let target = root.join("elsewhere");
    fs::write(&target, b"{}").unwrap();
    symlink(&target, &path).unwrap();
    assert_eq!(store.read().map(|_| ()), Err(StoreError::Symlink));
    fs::remove_file(&path).unwrap();
    mkfifo(&path);
    assert_eq!(store.read().map(|_| ()), Err(StoreError::NotRegularFile));
    fs::remove_file(&path).unwrap();
    write(&store, &set()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        store.read().map(|_| ()),
        Err(StoreError::PermissionsTooOpen { mode: 0o640 })
    );
    fs::write(&path, vec![b' '; 65_537]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(store.read().map(|_| ()), Err(StoreError::TooLarge));
    let euid = fs::metadata(&path).unwrap().uid();
    let foreign = Store::with_euid(root.join("store"), StoreMode::File, binding(), euid + 1);
    assert_eq!(foreign.read().map(|_| ()), Err(StoreError::DirectoryUnsafe));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-010"))]
#[test]
fn unsafe_store_directories_are_refused() {
    let root = temp_dir();
    let missing = Store::new(root.join("absent"), StoreMode::File, binding());
    assert_eq!(
        missing.read().map(|_| ()),
        Err(StoreError::DirectoryMissing)
    );
    assert!(!root.join("absent").exists());
    symlink(root.join("store"), root.join("link")).unwrap();
    let linked = Store::new(root.join("link"), StoreMode::File, binding());
    assert_eq!(linked.read().map(|_| ()), Err(StoreError::DirectoryUnsafe));
    fs::write(root.join("plainfile"), b"").unwrap();
    let notdir = Store::new(root.join("plainfile"), StoreMode::File, binding());
    assert_eq!(notdir.read().map(|_| ()), Err(StoreError::DirectoryUnsafe));
    fs::set_permissions(root.join("store"), fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        plain(&root).read().map(|_| ()),
        Err(StoreError::DirectoryUnsafe)
    );
    assert_eq!(
        plain(&root).lock(Duration::ZERO).map(|_| ()),
        Err(StoreError::DirectoryUnsafe)
    );
    assert!(!root.join("store").join("lock").exists());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-011"))]
#[test]
fn absent_store_means_signed_out() {
    let root = temp_dir();
    assert!(sealed(&root).read().unwrap().is_none());
    assert!(plain(&root).read().unwrap().is_none());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-012"))]
#[test]
fn malformed_token_sets_are_refused() {
    let root = temp_dir();
    let store = plain(&root);
    write(&store, &set()).unwrap();
    let path = root.join("store").join(FILE_NAME);
    let good = String::from_utf8(fs::read(&path).unwrap()).unwrap();
    let cases = [
        "not json".to_owned(),
        good.replacen('{', "{\"extra\":1,", 1),
        good.replace("\"version\":1", "\"version\":2"),
        good.replace(ACCESS, ""),
        good.replace(ACCESS, &"a".repeat(4097)),
        good.replace(ACCESS, "has space"),
        good.replace(REFRESH, "caf\u{e9}"),
    ];
    for case in cases {
        fs::write(&path, case.as_bytes()).unwrap();
        assert_eq!(store.read().map(|_| ()), Err(StoreError::Corrupt), "{case}");
    }
}

#[test]
fn inner_binding_differing_from_the_header_is_corrupt() {
    let root = temp_dir();
    let other = Binding::new("https://example.invalid/t", "c", "https://example.invalid/");
    // Seal under `other`, then rewrite the header to claim `binding()`.
    let store = Store::new(
        root.join("store"),
        StoreMode::Sealed(key_source(&root, KEY)),
        other,
    );
    write(&store, &set()).unwrap();
    let path = root.join("store").join(SEALED_NAME);
    let mut bytes = fs::read(&path).unwrap();
    bytes[8..40].copy_from_slice(binding().bytes());
    fs::write(&path, &bytes).unwrap();
    assert_eq!(sealed(&root).read().map(|_| ()), Err(StoreError::Corrupt));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-013"))]
#[test]
fn deleting_removes_every_copy() {
    let root = temp_dir();
    let dir = root.join("store");
    let store = sealed(&root);
    write(&store, &set()).unwrap();
    fs::write(dir.join(FILE_NAME), b"{}").unwrap();
    let lock = store.lock(Duration::ZERO).unwrap();
    store.delete(&lock).unwrap();
    assert!(!dir.join(FILE_NAME).exists() && !dir.join(SEALED_NAME).exists());
    store.delete(&lock).unwrap();
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-014"))]
#[test]
fn writers_hold_an_exclusive_lock() {
    let root = temp_dir();
    let (first, second) = (sealed(&root), sealed(&root));
    let held = first.lock(Duration::ZERO).unwrap();
    assert_eq!(
        second.lock(Duration::from_millis(250)).map(|_| ()),
        Err(StoreError::LockTimeout)
    );
    let started = Instant::now();
    let waiter = std::thread::spawn(move || second.lock(Duration::from_secs(5)).map(|_| ()));
    std::thread::sleep(Duration::from_millis(300));
    drop(held);
    assert_eq!(waiter.join().unwrap(), Ok(()));
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[test]
#[ignore = "helper run as a child process by the crash test"]
fn lock_holder_child() {
    let Some(dir) = std::env::var_os("STOCKHOUSE_LOCK_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let store = Store::new(dir.join("store"), StoreMode::File, binding());
    let _lock = store.lock(Duration::ZERO).unwrap();
    fs::write(dir.join("locked"), b"").unwrap();
    std::process::abort();
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-015"))]
#[test]
fn crashed_lock_holder_does_not_wedge_the_store() {
    let root = temp_dir();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lock_holder_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("STOCKHOUSE_LOCK_CHILD", &root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "the child aborted");
    assert!(root.join("locked").exists(), "the child held the lock");
    assert!(plain(&root).lock(Duration::ZERO).is_ok());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-016"))]
#[test]
fn stale_temporary_files_are_removed() {
    let root = temp_dir();
    let stale = root.join("store").join(".tokens.1.00ff.tmp");
    fs::write(&stale, b"partial").unwrap();
    write(&sealed(&root), &set()).unwrap();
    assert!(!stale.exists());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-STORE-017"))]
#[test]
fn unsafe_lock_files_are_refused() {
    let root = temp_dir();
    let store = plain(&root);
    let lock = root.join("store").join("lock");
    symlink(root.join("elsewhere"), &lock).unwrap();
    assert_eq!(
        store.lock(Duration::ZERO).map(|_| ()),
        Err(StoreError::Symlink)
    );
    fs::remove_file(&lock).unwrap();
    mkfifo(&lock);
    assert_eq!(
        store.lock(Duration::ZERO).map(|_| ()),
        Err(StoreError::NotRegularFile)
    );
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, b"").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        store.lock(Duration::ZERO).map(|_| ()),
        Err(StoreError::PermissionsTooOpen { mode: 0o640 })
    );
    assert!(!root.join("store").join(FILE_NAME).exists());
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SECRET-003"))]
#[test]
fn token_sets_are_redacted() {
    let text = format!("{:?}", set());
    assert!(text.contains("Secret(<redacted>)"));
    assert!(!text.contains(ACCESS) && !text.contains(REFRESH));
}

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use stockhouse_credentials::{
    CredentialError, CredentialSource, CredentialSpec, DeferredGrantFlow, GrantError, GrantFlow,
    OwnerOnlyFile, Secret, SystemdCredential, source,
};

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("stockhouse-creds-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &[u8], mode: u32) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn read(path: &Path) -> Result<Vec<u8>, CredentialError> {
    OwnerOnlyFile::new(path.to_path_buf())
        .credential()
        .map(|s| s.expose().to_vec())
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SOURCE-001"))]
#[test]
fn unconfigured_credentials_are_not_discovered() {
    assert_eq!(
        source(None).credential().map(|_| ()),
        Err(CredentialError::NotConfigured)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SOURCE-002"))]
#[test]
fn credentials_are_reread_on_every_use() {
    let path = temp_dir().join("token");
    write(&path, b"first\n", 0o600);
    let src = source(Some(&CredentialSpec::File(path.clone())));
    assert_eq!(src.credential().unwrap().expose(), b"first");
    write(&path, b"second", 0o600);
    assert_eq!(src.credential().unwrap().expose(), b"second");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SOURCE-003"))]
#[test]
fn systemd_credentials_skip_owner_and_mode_checks() {
    let dir = temp_dir();
    write(&dir.join("private"), b"tok\n", 0o440);
    let src = SystemdCredential::with_directory("private".to_owned(), Some(dir.clone()));
    assert_eq!(src.credential().unwrap().expose(), b"tok");
    symlink(dir.join("private"), dir.join("link")).unwrap();
    let link = SystemdCredential::with_directory("link".to_owned(), Some(dir.clone()));
    assert_eq!(link.credential().map(|_| ()), Err(CredentialError::Symlink));
    write(&dir.join("bad"), b"a b", 0o400);
    let bad = SystemdCredential::with_directory("bad".to_owned(), Some(dir));
    assert_eq!(
        bad.credential().map(|_| ()),
        Err(CredentialError::Malformed)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SOURCE-005"))]
#[test]
fn systemd_credentials_need_directory_and_plain_name() {
    let unset = SystemdCredential::with_directory("x".to_owned(), None);
    assert_eq!(
        unset.credential().map(|_| ()),
        Err(CredentialError::NotConfigured)
    );
    for name in ["", ".", "..", "a/b"] {
        let src = SystemdCredential::with_directory(name.to_owned(), Some(temp_dir()));
        assert_eq!(
            src.credential().map(|_| ()),
            Err(CredentialError::InvalidName),
            "{name:?}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SOURCE-004"))]
#[test]
fn grant_flow_is_deferred() {
    let flow = DeferredGrantFlow;
    let expected = Err(GrantError::Unavailable {
        dependency: "P2.06",
    });
    assert_eq!(flow.login(), expected);
    assert_eq!(flow.logout(), expected);
    assert_eq!(flow.refresh(), expected);
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-001"))]
#[test]
fn owner_only_file_yields_content() {
    let path = temp_dir().join("token");
    write(&path, b"abc-123\n", 0o600);
    assert_eq!(read(&path), Ok(b"abc-123".to_vec()));
    write(&path, b"abc-123", 0o400);
    assert_eq!(read(&path), Ok(b"abc-123".to_vec()));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-002"))]
#[test]
fn relative_and_store_paths_are_rejected() {
    assert_eq!(read(Path::new("token")), Err(CredentialError::RelativePath));
    assert_eq!(
        read(Path::new("/run/../nix/store/x")),
        Err(CredentialError::RelativePath)
    );
    assert_eq!(
        read(Path::new("/run/./x")),
        Err(CredentialError::RelativePath)
    );
    assert_eq!(
        read(Path::new("/nix/store/abc-token")),
        Err(CredentialError::InStore)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-003"))]
#[test]
fn symlinks_are_refused() {
    let dir = temp_dir();
    write(&dir.join("real"), b"tok", 0o600);
    symlink(dir.join("real"), dir.join("link")).unwrap();
    assert_eq!(read(&dir.join("link")), Err(CredentialError::Symlink));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-004"))]
#[test]
fn missing_and_non_regular_files_are_rejected() {
    let dir = temp_dir();
    assert_eq!(read(&dir.join("absent")), Err(CredentialError::Missing));
    assert_eq!(read(&dir), Err(CredentialError::NotRegularFile));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-005"))]
#[test]
fn open_permissions_are_rejected() {
    let path = temp_dir().join("token");
    for mode in [0o640, 0o604, 0o660] {
        write(&path, b"tok", mode);
        assert_eq!(
            read(&path),
            Err(CredentialError::PermissionsTooOpen { mode }),
            "{mode:o}"
        );
    }
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-006"))]
#[test]
fn files_owned_by_another_user_are_rejected() {
    let path = temp_dir().join("token");
    write(&path, b"tok", 0o600);
    // SAFETY: geteuid has no preconditions.
    let other = unsafe { libc::geteuid() }.wrapping_add(1);
    let src = OwnerOnlyFile::with_euid(path, other);
    assert_eq!(
        src.credential().map(|_| ()),
        Err(CredentialError::WrongOwner)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-007"))]
#[test]
fn empty_oversized_and_malformed_content_is_rejected() {
    let path = temp_dir().join("token");
    for (content, error) in [
        (b"\n".to_vec(), CredentialError::Empty),
        (Vec::new(), CredentialError::Empty),
        (vec![b'a'; 4097], CredentialError::TooLarge),
        (b"has space".to_vec(), CredentialError::Malformed),
        (b"two\n\n".to_vec(), CredentialError::Malformed),
    ] {
        write(&path, &content, 0o600);
        assert_eq!(read(&path), Err(error));
    }
    write(&path, &[b'a'; 4096], 0o600);
    assert_eq!(read(&path).map(|v| v.len()), Ok(4096));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-FILE-008"))]
#[test]
fn fifo_never_blocks() {
    let path = temp_dir().join("fifo");
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: c_path is a valid NUL-terminated string.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert_eq!(read(&path), Err(CredentialError::NotRegularFile));
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SECRET-001"))]
#[test]
fn secrets_are_redacted() {
    let secret = Secret::new(b"hunter2-token".to_vec());
    assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    assert_eq!(format!("{secret}"), "Secret(<redacted>)");
    assert_eq!(secret.expose(), b"hunter2-token");
}

#[cfg_attr(any(), bloomery("CLIENT-CREDS-SECRET-002"))]
#[test]
fn errors_never_contain_content() {
    let path = temp_dir().join("token");
    for (content, mode) in [(&b"leak me"[..], 0o600), (b"leakme", 0o644)] {
        write(&path, content, mode);
        let error = read(&path).unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(!text.contains("leak"), "{text}");
    }
}

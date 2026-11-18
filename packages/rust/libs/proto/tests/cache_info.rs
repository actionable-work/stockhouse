use stockhouse_proto::{CacheInfoError, NixCacheInfo};

fn parse(text: &str) -> Result<NixCacheInfo, CacheInfoError> {
    NixCacheInfo::parse(text.as_bytes())
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-001"))]
#[test]
fn valid_cache_info_parses() {
    let info = parse("StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n").unwrap();
    assert_eq!(
        info,
        NixCacheInfo {
            want_mass_query: Some(true),
            priority: Some(40)
        }
    );
    assert_eq!(
        parse("StoreDir: /nix/store\n").unwrap(),
        NixCacheInfo::default()
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-002"))]
#[test]
fn foreign_store_dirs_are_rejected() {
    assert_eq!(
        parse("StoreDir: /gnu/store\n"),
        Err(CacheInfoError::StoreDirMismatch {
            found: "/gnu/store".to_owned()
        })
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-003"))]
#[test]
fn unknown_duplicate_and_missing_fields_are_rejected() {
    assert_eq!(
        parse("StoreDir: /nix/store\nFoo: 1\n"),
        Err(CacheInfoError::UnknownField("Foo".to_owned()))
    );
    assert_eq!(
        parse("StoreDir: /nix/store\nPriority: 1\nPriority: 2\n"),
        Err(CacheInfoError::DuplicateField("Priority"))
    );
    assert_eq!(
        parse("Priority: 1\n"),
        Err(CacheInfoError::MissingField("StoreDir"))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-004"))]
#[test]
fn invalid_values_are_rejected() {
    assert_eq!(
        parse("StoreDir: /nix/store\nWantMassQuery: yes\n"),
        Err(CacheInfoError::InvalidField("WantMassQuery"))
    );
    assert_eq!(
        parse("StoreDir: /nix/store\nPriority: 4294967296\n"),
        Err(CacheInfoError::InvalidField("Priority"))
    );
    assert_eq!(
        parse("StoreDir: /nix/store\nPriority: -1\n"),
        Err(CacheInfoError::InvalidField("Priority"))
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-006"))]
#[test]
fn malformed_input_is_rejected() {
    assert_eq!(
        parse("StoreDir /nix/store\n"),
        Err(CacheInfoError::MalformedLine { line: 1 })
    );
    let big = format!("StoreDir: /nix/store\n{}", "\n".repeat(4096));
    assert_eq!(parse(&big), Err(CacheInfoError::TooLarge));
    assert_eq!(
        NixCacheInfo::parse(b"StoreDir: \xff\n"),
        Err(CacheInfoError::NotUtf8)
    );
}

#[cfg_attr(any(), bloomery("CLIENT-PROTO-CACHEINFO-005"))]
#[test]
fn render_round_trips() {
    for info in [
        NixCacheInfo::default(),
        NixCacheInfo {
            want_mass_query: Some(false),
            priority: Some(30),
        },
    ] {
        assert_eq!(parse(&info.render()).unwrap(), info);
    }
}

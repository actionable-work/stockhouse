//! Argument parsing and the usage exit status.
use std::path::PathBuf;
use std::process::ExitCode;
use stockhouse_fake_server_cli::{Mode, Usage, main, parse};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|a| (*a).to_owned()).collect()
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-002"))]
#[test]
fn usage_errors_are_exact() {
    let cases: [(&[&str], Usage); 7] = [
        (&[], Usage::NoCommand),
        (&["serve"], Usage::UnknownCommand("serve".into())),
        (
            &["oauth", "--root", "/x"],
            Usage::UnknownFlag("--root".into()),
        ),
        (
            &["oauth", "--listen"],
            Usage::MissingValue("--listen".into()),
        ),
        (
            &["upload", "--listen", "127.0.0.1:1"],
            Usage::Missing("--root"),
        ),
        (&["oauth", "--listen", "localhost"], Usage::BadAddress),
        (
            &[
                "upload",
                "--listen",
                "127.0.0.1:1",
                "--root",
                "/a",
                "--root",
                "/b",
            ],
            Usage::Repeated("--root".into()),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(&args(input)), Err(expected), "{input:?}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-002"))]
#[test]
fn main_exits_2_on_usage_errors() {
    for input in [
        &["stockhouse-fake-server"][..],
        &["stockhouse-fake-server", "bogus"],
    ] {
        assert_eq!(main(args(input)), ExitCode::from(2), "{input:?}");
    }
}

#[cfg_attr(any(), bloomery("CLIENT-INTEGRATION-FAKE-002"))]
#[test]
fn modes_parse_with_their_flags() {
    let listen = "127.0.0.1:8486".parse().unwrap();
    assert_eq!(
        parse(&args(&["oauth", "--listen", "127.0.0.1:8486"])),
        Ok(Mode::OAuth {
            listen,
            cache_dir: None
        })
    );
    assert_eq!(
        parse(&args(&[
            "upload",
            "--token-file",
            "/t",
            "--listen",
            "127.0.0.1:8486",
            "--root",
            "/r"
        ])),
        Ok(Mode::Upload {
            listen,
            root: PathBuf::from("/r"),
            token_file: PathBuf::from("/t"),
        })
    );
    assert_eq!(
        parse(&args(&[
            "shared",
            "--listen",
            "127.0.0.1:8486",
            "--log",
            "/l",
            "--deny",
            "a",
            "--fail",
            "b",
            "--deny",
            "c",
        ])),
        Ok(Mode::Shared {
            listen,
            log: PathBuf::from("/l"),
            deny: vec!["a".into(), "c".into()],
            fail: vec!["b".into()],
        })
    );
}

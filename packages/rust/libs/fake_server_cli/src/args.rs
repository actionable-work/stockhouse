use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

/// A fake server mode with its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The sign-in stories' OAuth endpoints and private cache.
    OAuth {
        /// Listen address.
        listen: SocketAddr,
        /// Files served under `/t/1/`.
        cache_dir: Option<PathBuf>,
    },
    /// The upload stories' protocol v1 fake.
    Upload {
        /// Listen address.
        listen: SocketAddr,
        /// State, logs and control files.
        root: PathBuf,
        /// The file holding the expected bearer token.
        token_file: PathBuf,
    },
    /// The shared-tier fixture.
    Shared {
        /// Listen address.
        listen: SocketAddr,
        /// The request log.
        log: PathBuf,
        /// Hashes whose narinfo answers 403.
        deny: Vec<String>,
        /// Hashes whose narinfo answers 500.
        fail: Vec<String>,
    },
}

impl Mode {
    /// The address to listen on.
    pub fn listen(&self) -> SocketAddr {
        match self {
            Self::OAuth { listen, .. }
            | Self::Upload { listen, .. }
            | Self::Shared { listen, .. } => *listen,
        }
    }
}

/// Why the arguments were refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usage {
    /// No command was given.
    NoCommand,
    /// The command is not `oauth`, `upload` or `shared`.
    UnknownCommand(String),
    /// The flag is not accepted by the command.
    UnknownFlag(String),
    /// The flag has no value.
    MissingValue(String),
    /// A required flag is absent.
    Missing(&'static str),
    /// A single-valued flag was given twice.
    Repeated(String),
    /// `--listen` is not a socket address.
    BadAddress,
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCommand => write!(f, "no command"),
            Self::UnknownCommand(c) => write!(f, "unknown command {c:?}"),
            Self::UnknownFlag(flag) => write!(f, "unknown flag {flag:?}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
            Self::Missing(flag) => write!(f, "{flag} is required"),
            Self::Repeated(flag) => write!(f, "{flag} is given twice"),
            Self::BadAddress => write!(f, "--listen is not an address"),
        }
    }
}

type Flags = BTreeMap<String, Vec<String>>;

const REPEATABLE: [&str; 2] = ["--deny", "--fail"];

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Mode, Usage> {
    let (command, rest) = args.split_first().ok_or(Usage::NoCommand)?;
    let allowed: &[&str] = match command.as_str() {
        "oauth" => &["--listen", "--cache-dir"],
        "upload" => &["--listen", "--root", "--token-file"],
        "shared" => &["--listen", "--log", "--deny", "--fail"],
        other => return Err(Usage::UnknownCommand(other.to_owned())),
    };
    let mut flags = flags(rest, allowed)?;
    let listen = required(&mut flags, "--listen")?
        .parse()
        .map_err(|_| Usage::BadAddress)?;
    Ok(match command.as_str() {
        "oauth" => Mode::OAuth {
            listen,
            cache_dir: flags
                .remove("--cache-dir")
                .map(|mut v| PathBuf::from(v.remove(0))),
        },
        "upload" => Mode::Upload {
            listen,
            root: required(&mut flags, "--root")?.into(),
            token_file: required(&mut flags, "--token-file")?.into(),
        },
        _ => Mode::Shared {
            listen,
            log: required(&mut flags, "--log")?.into(),
            deny: flags.remove("--deny").unwrap_or_default(),
            fail: flags.remove("--fail").unwrap_or_default(),
        },
    })
}

fn flags(rest: &[String], allowed: &[&str]) -> Result<Flags, Usage> {
    let mut flags = Flags::new();
    let mut items = rest.iter();
    while let Some(flag) = items.next() {
        if !allowed.contains(&flag.as_str()) {
            return Err(Usage::UnknownFlag(flag.clone()));
        }
        let value = items
            .next()
            .ok_or_else(|| Usage::MissingValue(flag.clone()))?;
        let values = flags.entry(flag.clone()).or_default();
        if !values.is_empty() && !REPEATABLE.contains(&flag.as_str()) {
            return Err(Usage::Repeated(flag.clone()));
        }
        values.push(value.clone());
    }
    Ok(flags)
}

fn required(flags: &mut Flags, name: &'static str) -> Result<String, Usage> {
    flags
        .remove(name)
        .map(|mut v| v.remove(0))
        .ok_or(Usage::Missing(name))
}

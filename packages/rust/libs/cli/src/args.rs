use std::fmt;
use std::path::PathBuf;

/// A parsed command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Parsed {
    /// `proxy --config <file>`.
    Proxy { config: PathBuf },
    /// `uploader --config <file> [--once]`.
    Uploader { config: PathBuf, once: bool },
    /// `post-build-hook --queue <dir>`.
    Hook { queue: PathBuf },
    /// `status --queue <dir>`.
    Status { queue: PathBuf },
    /// `login`, `logout` or `refresh`.
    Grant(&'static str),
    /// `--version`.
    Version,
}

/// A usage error. Values are never echoed, since they could be secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// No command.
    MissingCommand,
    /// An unknown command.
    UnknownCommand,
    /// An unknown or repeated flag.
    UnknownFlag(String),
    /// A flag lacks its value.
    MissingValue(&'static str),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCommand => f.write_str("missing command"),
            Self::UnknownCommand => f.write_str("unknown command"),
            Self::UnknownFlag(flag) => write!(f, "unknown flag {flag}"),
            Self::MissingValue(flag) => write!(f, "{flag} needs a value"),
        }
    }
}

fn flag_name(arg: &str) -> String {
    let name = arg.split('=').next().unwrap_or_default();
    if name.starts_with("--") {
        name.to_owned()
    } else {
        "<positional>".to_owned()
    }
}

fn flags(
    rest: &[String],
    value_flag: &'static str,
    allow_once: bool,
) -> Result<(PathBuf, bool), UsageError> {
    let (mut value, mut once) = (None, false);
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if arg == value_flag && value.is_none() {
            value = Some(PathBuf::from(
                iter.next().ok_or(UsageError::MissingValue(value_flag))?,
            ));
        } else if allow_once && arg == "--once" && !once {
            once = true;
        } else {
            return Err(UsageError::UnknownFlag(flag_name(arg)));
        }
    }
    Ok((value.ok_or(UsageError::MissingValue(value_flag))?, once))
}

/// Parses arguments after the program name.
pub fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    let (command, rest) = args.split_first().ok_or(UsageError::MissingCommand)?;
    let no_flags = |parsed: Parsed| match rest.first() {
        Some(extra) => Err(UsageError::UnknownFlag(flag_name(extra))),
        None => Ok(parsed),
    };
    match command.as_str() {
        "proxy" => Ok(Parsed::Proxy {
            config: flags(rest, "--config", false)?.0,
        }),
        "uploader" => {
            let (config, once) = flags(rest, "--config", true)?;
            Ok(Parsed::Uploader { config, once })
        }
        "post-build-hook" => Ok(Parsed::Hook {
            queue: flags(rest, "--queue", false)?.0,
        }),
        "status" => Ok(Parsed::Status {
            queue: flags(rest, "--queue", false)?.0,
        }),
        "login" => no_flags(Parsed::Grant("login")),
        "logout" => no_flags(Parsed::Grant("logout")),
        "refresh" => no_flags(Parsed::Grant("refresh")),
        "--version" => no_flags(Parsed::Version),
        _ => Err(UsageError::UnknownCommand),
    }
}

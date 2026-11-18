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
    /// `status --config <auth.toml>`.
    SessionStatus { config: PathBuf },
    /// `login --config <auth.toml> [--device]`.
    Login { config: PathBuf, device: bool },
    /// `logout --config <auth.toml>`.
    Logout { config: PathBuf },
    /// `refresh --config <auth.toml>`.
    Refresh { config: PathBuf },
    /// `lease --config <file>`.
    Lease { config: PathBuf },
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
    switch: Option<&'static str>,
) -> Result<(PathBuf, bool), UsageError> {
    let (mut value, mut once) = (None, false);
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if arg == value_flag && value.is_none() {
            value = Some(PathBuf::from(
                iter.next().ok_or(UsageError::MissingValue(value_flag))?,
            ));
        } else if switch == Some(arg.as_str()) && !once {
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
    let config = || flags(rest, "--config", None).map(|(config, _)| config);
    match command.as_str() {
        "proxy" => Ok(Parsed::Proxy { config: config()? }),
        "uploader" => {
            let (config, once) = flags(rest, "--config", Some("--once"))?;
            Ok(Parsed::Uploader { config, once })
        }
        "post-build-hook" => Ok(Parsed::Hook {
            queue: flags(rest, "--queue", None)?.0,
        }),
        "status" => status(rest),
        "lease" => Ok(Parsed::Lease { config: config()? }),
        "login" => {
            let (config, device) = flags(rest, "--config", Some("--device"))?;
            Ok(Parsed::Login { config, device })
        }
        "logout" => Ok(Parsed::Logout { config: config()? }),
        "refresh" => Ok(Parsed::Refresh { config: config()? }),
        "--version" => match rest.first() {
            Some(extra) => Err(UsageError::UnknownFlag(flag_name(extra))),
            None => Ok(Parsed::Version),
        },
        _ => Err(UsageError::UnknownCommand),
    }
}

/// `status` takes exactly one of `--queue <dir>` and `--config <file>`.
fn status(rest: &[String]) -> Result<Parsed, UsageError> {
    if rest.first().map(String::as_str) == Some("--config") {
        let config = flags(rest, "--config", None)?.0;
        return Ok(Parsed::SessionStatus { config });
    }
    Ok(Parsed::Status {
        queue: flags(rest, "--queue", None)?.0,
    })
}

//! The server sign-in commands. Output names outcomes only, never a token.

use crate::EXIT_USAGE;
use crate::commands::EXIT_FAILURE;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use stockhouse_auth::{AuthConfig, LogoutOutcome, Replaced, ServerCredential, SystemClock};
use stockhouse_credentials::CredentialError;
use stockhouse_signin::{Flow, Options};

const SIGN_IN_AGAIN: &str = "run `stockhouse-auth login` to sign in again";

fn load(path: &Path, command: &str, err: &mut dyn Write) -> Option<AuthConfig> {
    match AuthConfig::load(path) {
        Ok(config) => Some(config),
        Err(error) => {
            let _ = writeln!(err, "stockhouse {command}: {error}");
            None
        }
    }
}

pub(crate) fn login(path: &Path, device: bool, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some(config) = load(path, "login", err) else {
        return EXIT_USAGE;
    };
    let flow = if device { Flow::Device } else { Flow::Loopback };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime starts");
    let result = runtime.block_on(stockhouse_signin::login(
        &config,
        flow,
        &Options::default(),
        out,
    ));
    match result {
        Ok(replaced) => {
            if let Replaced::RevocationFailed(failure) = replaced {
                let _ = writeln!(
                    err,
                    "stockhouse login: warning: the previous sign-in could not be revoked: {failure}"
                );
            }
            let _ = writeln!(out, "Signed in.");
            0
        }
        Err(error) => {
            let _ = writeln!(err, "stockhouse login: {error}");
            EXIT_FAILURE
        }
    }
}

pub(crate) fn logout(path: &Path, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some(config) = load(path, "logout", err) else {
        return EXIT_USAGE;
    };
    let message = match stockhouse_auth::logout(&config) {
        Ok(LogoutOutcome::NotSignedIn) => "Not signed in.",
        Ok(LogoutOutcome::Revoked) => "Signed out.",
        Ok(LogoutOutcome::NothingRevoked) => "Signed out (nothing to revoke).",
        Ok(LogoutOutcome::RevocationFailed(failure)) => {
            let _ = writeln!(
                err,
                "stockhouse logout: signed out locally, but revoking the grant failed: {failure}"
            );
            return EXIT_FAILURE;
        }
        Err(error) => {
            let _ = writeln!(err, "stockhouse logout: {error}");
            return EXIT_FAILURE;
        }
    };
    let _ = writeln!(out, "{message}");
    0
}

pub(crate) fn refresh(path: &Path, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some(config) = load(path, "refresh", err) else {
        return EXIT_USAGE;
    };
    let credential = ServerCredential::new(Arc::new(config), Arc::new(SystemClock));
    match credential.force_refresh() {
        Ok(_) => {
            let _ = writeln!(out, "Refreshed.");
            0
        }
        Err(CredentialError::SignedOut) => {
            let _ = writeln!(err, "stockhouse refresh: signed out: {SIGN_IN_AGAIN}");
            EXIT_FAILURE
        }
        Err(error) => {
            let _ = writeln!(err, "stockhouse refresh: {error}");
            EXIT_FAILURE
        }
    }
}

pub(crate) fn status(path: &Path, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let Some(config) = load(path, "status", err) else {
        return EXIT_USAGE;
    };
    let status = stockhouse_auth::status(&config, &SystemClock);
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string(&status).expect("status serializes")
    );
    if status.signed_in { 0 } else { EXIT_FAILURE }
}

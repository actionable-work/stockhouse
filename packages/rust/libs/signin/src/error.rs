use std::fmt;
use stockhouse_auth::{Failure, OAuthCode};
use stockhouse_token_store::StoreError;

/// Why a sign-in failed. Carries classifications only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SigninError {
    /// The token store is unusable.
    Store(StoreError),
    /// The callback's `state` was missing or wrong.
    StateMismatch,
    /// The authorization server answered with an error.
    AuthorizationDenied(OAuthCode),
    /// The callback carried no usable code.
    InvalidCallback,
    /// No callback arrived in time.
    LoginTimeout,
    /// The loopback listener could not be bound.
    Listener,
    /// The device authorization response was malformed.
    DeviceResponse,
    /// The user denied the device request.
    DeviceDenied,
    /// The device code expired.
    DeviceExpired,
    /// The token or device endpoint failed.
    TokenEndpoint(Failure),
    /// Randomness was unavailable.
    Random,
}

impl fmt::Display for SigninError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "{error}"),
            Self::StateMismatch => f.write_str("sign-in callback state did not match"),
            Self::AuthorizationDenied(code) => {
                write!(f, "sign-in refused by the server ({})", code.as_str())
            }
            Self::InvalidCallback => f.write_str("sign-in callback had no valid code"),
            Self::LoginTimeout => f.write_str("sign-in timed out waiting for the browser"),
            Self::Listener => f.write_str("cannot listen on 127.0.0.1"),
            Self::DeviceResponse => f.write_str("invalid device authorization response"),
            Self::DeviceDenied => f.write_str("sign-in was denied"),
            Self::DeviceExpired => f.write_str("the device code expired"),
            Self::TokenEndpoint(failure) => write!(f, "token endpoint: {failure}"),
            Self::Random => f.write_str("no randomness available"),
        }
    }
}

impl std::error::Error for SigninError {}

use std::fmt;

/// A grant-flow operation is not available in this build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GrantError {
    /// The server grant flow this needs is not implemented yet.
    Unavailable {
        /// The plan slice that provides it.
        dependency: &'static str,
    },
}

impl fmt::Display for GrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Unavailable { dependency } = self;
        write!(f, "sign-in is not available yet: it requires server grants ({dependency})")
    }
}

impl std::error::Error for GrantError {}

/// Server sign-in, sign-out and refresh.
pub trait GrantFlow {
    /// Signs in.
    fn login(&self) -> Result<(), GrantError>;
    /// Signs out.
    fn logout(&self) -> Result<(), GrantError>;
    /// Refreshes the current grant.
    fn refresh(&self) -> Result<(), GrantError>;
}

/// The only grant flow in this slice; real flows arrive with P2.06.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeferredGrantFlow;

const UNAVAILABLE: GrantError = GrantError::Unavailable { dependency: "P2.06" };

impl GrantFlow for DeferredGrantFlow {
    fn login(&self) -> Result<(), GrantError> {
        Err(UNAVAILABLE)
    }

    fn logout(&self) -> Result<(), GrantError> {
        Err(UNAVAILABLE)
    }

    fn refresh(&self) -> Result<(), GrantError> {
        Err(UNAVAILABLE)
    }
}

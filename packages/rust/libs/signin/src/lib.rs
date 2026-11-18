//! Interactive server sign-in: RFC 8252 loopback authorization code with
//! S256 PKCE (RFC 7636) and the RFC 8628 device flow. Used by the CLI only.

mod device;
mod error;
mod loopback;
mod pkce;

pub use device::DeviceLogin;
pub use error::SigninError;
pub use loopback::{LOGIN_WAIT, LoopbackLogin, callback_outcome};
pub use pkce::Pkce;

use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use stockhouse_auth::{AuthConfig, Clock, Replaced, SystemClock};

/// Waits between device polls.
pub trait Pacer: Send + Sync {
    /// Waits `duration`.
    fn wait(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Real time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sleep;

impl Pacer for Sleep {
    fn wait(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Which flow to run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Flow {
    /// Loopback authorization code with PKCE.
    Loopback,
    /// Device authorization.
    Device,
}

/// Injectable time for login.
#[derive(Clone)]
pub struct Options {
    /// The loopback wait (default 300 s).
    pub wait: Duration,
    /// The device poll pacer.
    pub pacer: Arc<dyn Pacer>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            wait: LOGIN_WAIT,
            pacer: Arc::new(Sleep),
            clock: Arc::new(SystemClock),
        }
    }
}

/// Runs one sign-in flow, printing its prompts to `out`, and stores the
/// tokens. Returns what happened to a replaced session.
pub async fn login<W: Write + ?Sized>(
    config: &AuthConfig,
    flow: Flow,
    options: &Options,
    out: &mut W,
) -> Result<Replaced, SigninError> {
    config
        .store()
        .preflight(config.lock_wait())
        .map_err(SigninError::Store)?;
    let set = match flow {
        Flow::Loopback => {
            LoopbackLogin::start(config)
                .await?
                .run(options, out)
                .await?
        }
        Flow::Device => DeviceLogin::new(config, options).run(out).await?,
    };
    stockhouse_auth::store_login(config, &set)
        .await
        .map_err(SigninError::Store)
}

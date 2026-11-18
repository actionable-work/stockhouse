use crate::body::ByteStream;
use crate::config::{ProxyConfig, TierConfig, TierName, TierUrl};
use crate::error::{ProxyError, ServeError};
use crate::upstream::{Fetched, Upstream, UpstreamError};
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stockhouse_auth::Source;
use stockhouse_credentials::{CredentialSource, CredentialSpec, Secret};
use stockhouse_proto::{NarInfo, NixCacheInfo, StorePathHash, TrustedKeys};

const NARINFO_LIMIT: u64 = 1024 * 1024;
const CACHE_INFO_LIMIT: u64 = 4096;

/// One configured tier with its transport, keys and credential source.
pub struct Tier {
    name: TierName,
    upstream: Upstream,
    keys: TrustedKeys,
    credential: Option<Arc<dyn CredentialSource>>,
    cache_info_ok: AtomicBool,
}

/// The credential state of one proxy request: the secret in use and
/// whether its one 401 retry has been spent.
pub(crate) struct Attempt {
    secret: Option<Secret>,
    retried: bool,
}

/// A verified narinfo with the credential state used to fetch it.
pub(crate) struct Resolved {
    pub(crate) narinfo: NarInfo,
    pub(crate) attempt: Attempt,
}

fn build_source(
    name: TierName,
    url: &TierUrl,
    spec: &CredentialSpec,
) -> Result<Arc<dyn CredentialSource>, ServeError> {
    let source = stockhouse_auth::source(Some(spec))
        .map_err(|error| ServeError::Credential { tier: name, error })?;
    if let (Source::Server(server), TierUrl::Http(tier_url)) = (&source, url) {
        // Both are parsed URLs, so this compares normalized forms.
        if server.private_tier() != tier_url {
            return Err(ServeError::CredentialAudience { tier: name });
        }
    }
    Ok(source.shared())
}

impl Tier {
    fn new(config: &TierConfig, timeout: Duration) -> Result<Self, ServeError> {
        let upstream = match &config.url {
            TierUrl::File(dir) => Upstream::File(dir.clone()),
            TierUrl::Http(url) => Upstream::http(url.clone(), timeout)
                .map_err(|e| ServeError::Client(e.to_string()))?,
        };
        let credential = config
            .credential
            .as_ref()
            .map(|spec| build_source(config.name, &config.url, spec))
            .transpose()?;
        Ok(Self {
            name: config.name,
            upstream,
            keys: config.keys.clone(),
            credential,
            cache_info_ok: AtomicBool::new(false),
        })
    }

    /// Which tier this is.
    pub fn name(&self) -> TierName {
        self.name
    }

    fn fail(&self, error: UpstreamError) -> ProxyError {
        match error {
            UpstreamError::Denied | UpstreamError::Unauthorized => {
                ProxyError::Denied { tier: self.name }
            }
            reason => ProxyError::Unavailable {
                tier: self.name,
                reason,
            },
        }
    }

    /// Resolves the credential on the blocking pool: it may block.
    async fn secret(&self) -> Result<Option<Secret>, ProxyError> {
        let Some(source) = self.credential.clone() else {
            return Ok(None);
        };
        let tier = self.name;
        let resolved = tokio::task::spawn_blocking(move || source.credential())
            .await
            .unwrap_or(Err(stockhouse_credentials::CredentialError::Unreadable));
        resolved
            .map(Some)
            .map_err(|error| ProxyError::Credential { tier, error })
    }

    /// After a 401: spends the one retry if the source offers a new
    /// credential, replacing the attempt's secret.
    async fn invalidated(&self, attempt: &mut Attempt) -> Result<bool, ProxyError> {
        if attempt.retried {
            return Ok(false);
        }
        let (Some(source), Some(sent)) = (self.credential.clone(), attempt.secret.take()) else {
            return Ok(false);
        };
        attempt.retried = true;
        let joined = tokio::task::spawn_blocking(move || {
            let again = source.invalidate(&sent);
            (again, sent)
        })
        .await;
        let Ok((again, sent)) = joined else {
            return Ok(false);
        };
        if again {
            attempt.secret = self.secret().await?;
        } else {
            attempt.secret = Some(sent);
        }
        Ok(again)
    }

    async fn fetch(
        &self,
        attempt: &mut Attempt,
        rel: &str,
        limit: u64,
    ) -> Result<Fetched<Bytes>, ProxyError> {
        match self
            .upstream
            .fetch(rel, attempt.secret.as_ref(), limit)
            .await
        {
            Err(UpstreamError::Unauthorized) if self.invalidated(attempt).await? => self
                .upstream
                .fetch(rel, attempt.secret.as_ref(), limit)
                .await
                .map_err(|e| self.fail(e)),
            other => other.map_err(|e| self.fail(e)),
        }
    }

    /// Opens a NAR, retrying a 401 before any byte is sent.
    pub(crate) async fn open(
        &self,
        attempt: &mut Attempt,
        rel: &str,
    ) -> Result<Fetched<(ByteStream, Option<u64>)>, ProxyError> {
        match self.upstream.open(rel, attempt.secret.as_ref()).await {
            Err(UpstreamError::Unauthorized) if self.invalidated(attempt).await? => self
                .upstream
                .open(rel, attempt.secret.as_ref())
                .await
                .map_err(|e| self.fail(e)),
            other => other.map_err(|e| self.fail(e)),
        }
    }

    async fn check_cache_info(&self, attempt: &mut Attempt) -> Result<(), ProxyError> {
        if self.cache_info_ok.load(Ordering::Acquire) {
            return Ok(());
        }
        let fetched = self
            .fetch(attempt, "nix-cache-info", CACHE_INFO_LIMIT)
            .await?;
        let Fetched::Found(body) = fetched else {
            return Err(ProxyError::CacheInfoMissing { tier: self.name });
        };
        NixCacheInfo::parse(&body).map_err(|error| ProxyError::CacheInfo {
            tier: self.name,
            error,
        })?;
        self.cache_info_ok.store(true, Ordering::Release);
        Ok(())
    }

    /// Lookup steps 1–5 on this tier: `Ok(None)` is a genuine miss.
    pub(crate) async fn resolve(
        &self,
        hash: &StorePathHash,
    ) -> Result<Option<Resolved>, ProxyError> {
        let mut attempt = Attempt {
            secret: self.secret().await?,
            retried: false,
        };
        self.check_cache_info(&mut attempt).await?;
        let path = format!("{hash}.narinfo");
        let Fetched::Found(body) = self.fetch(&mut attempt, &path, NARINFO_LIMIT).await? else {
            return Ok(None);
        };
        let narinfo = NarInfo::parse(&body).map_err(|error| ProxyError::NarInfo {
            tier: self.name,
            error,
        })?;
        if narinfo.store_path.hash() != *hash {
            return Err(ProxyError::StorePathMismatch { tier: self.name });
        }
        self.keys
            .verify(&narinfo)
            .map_err(|error| ProxyError::Signature {
                tier: self.name,
                error,
            })?;
        Ok(Some(Resolved { narinfo, attempt }))
    }
}

/// The ordered tier policy.
pub struct Lookup {
    tiers: Vec<Tier>,
}

impl Lookup {
    /// Builds tiers from configuration.
    pub fn new(config: &ProxyConfig) -> Result<Self, ServeError> {
        let tiers = config
            .tiers
            .iter()
            .map(|t| Tier::new(t, config.timeout))
            .collect::<Result<_, _>>()?;
        Ok(Self { tiers })
    }

    /// The configured tier with this name.
    pub fn tier(&self, name: TierName) -> Option<&Tier> {
        self.tiers.iter().find(|t| t.name == name)
    }

    /// Consults tiers in order; only a genuine miss moves to the next tier.
    pub async fn find(
        &self,
        hash: &StorePathHash,
    ) -> Result<Option<(TierName, NarInfo)>, ProxyError> {
        for tier in &self.tiers {
            if let Some(found) = tier.resolve(hash).await? {
                return Ok(Some((tier.name, found.narinfo)));
            }
        }
        Ok(None)
    }
}

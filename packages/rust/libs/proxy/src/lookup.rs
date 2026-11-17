use crate::config::{ProxyConfig, TierConfig, TierName, TierUrl};
use crate::error::{ProxyError, ServeError};
use crate::upstream::{Fetched, Upstream, UpstreamError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use stockhouse_credentials::{CredentialSource, Secret};
use stockhouse_proto::{NarInfo, NixCacheInfo, StorePathHash, TrustedKeys};

const NARINFO_LIMIT: u64 = 1024 * 1024;
const CACHE_INFO_LIMIT: u64 = 4096;

/// One configured tier with its transport, keys and credential source.
pub struct Tier {
    name: TierName,
    upstream: Upstream,
    keys: TrustedKeys,
    credential: Option<Box<dyn CredentialSource>>,
    cache_info_ok: AtomicBool,
}

/// A verified narinfo with the credential used to fetch it.
pub(crate) struct Resolved {
    pub(crate) narinfo: NarInfo,
    pub(crate) auth: Option<Secret>,
}

impl Tier {
    fn new(config: &TierConfig, timeout: Duration) -> Result<Self, ServeError> {
        let upstream = match &config.url {
            TierUrl::File(dir) => Upstream::File(dir.clone()),
            TierUrl::Http(url) => Upstream::http(url.clone(), timeout).map_err(|e| ServeError::Client(e.to_string()))?,
        };
        let credential = config.credential.as_ref().map(|spec| stockhouse_credentials::source(Some(spec)));
        let cache_info_ok = AtomicBool::new(false);
        Ok(Self { name: config.name, upstream, keys: config.keys.clone(), credential, cache_info_ok })
    }

    /// Which tier this is.
    pub fn name(&self) -> TierName {
        self.name
    }

    fn fail(&self, error: UpstreamError) -> ProxyError {
        match error {
            UpstreamError::Denied => ProxyError::Denied { tier: self.name },
            reason => ProxyError::Unavailable { tier: self.name, reason },
        }
    }

    fn secret(&self) -> Result<Option<Secret>, ProxyError> {
        let Some(source) = &self.credential else { return Ok(None) };
        source.credential().map(Some).map_err(|error| ProxyError::Credential { tier: self.name, error })
    }

    async fn check_cache_info(&self, auth: Option<&Secret>) -> Result<(), ProxyError> {
        if self.cache_info_ok.load(Ordering::Acquire) {
            return Ok(());
        }
        let fetched = self.upstream.fetch("nix-cache-info", auth, CACHE_INFO_LIMIT).await.map_err(|e| self.fail(e))?;
        let Fetched::Found(body) = fetched else { return Err(ProxyError::CacheInfoMissing { tier: self.name }) };
        NixCacheInfo::parse(&body).map_err(|error| ProxyError::CacheInfo { tier: self.name, error })?;
        self.cache_info_ok.store(true, Ordering::Release);
        Ok(())
    }

    /// Lookup steps 1–5 on this tier: `Ok(None)` is a genuine miss.
    pub(crate) async fn resolve(&self, hash: &StorePathHash) -> Result<Option<Resolved>, ProxyError> {
        let auth = self.secret()?;
        self.check_cache_info(auth.as_ref()).await?;
        let path = format!("{hash}.narinfo");
        let fetched = self.upstream.fetch(&path, auth.as_ref(), NARINFO_LIMIT).await.map_err(|e| self.fail(e))?;
        let Fetched::Found(body) = fetched else { return Ok(None) };
        let narinfo = NarInfo::parse(&body).map_err(|error| ProxyError::NarInfo { tier: self.name, error })?;
        if narinfo.store_path.hash() != *hash {
            return Err(ProxyError::StorePathMismatch { tier: self.name });
        }
        self.keys.verify(&narinfo).map_err(|error| ProxyError::Signature { tier: self.name, error })?;
        Ok(Some(Resolved { narinfo, auth }))
    }

    pub(crate) fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    pub(crate) fn fail_upstream(&self, error: UpstreamError) -> ProxyError {
        self.fail(error)
    }
}

/// The ordered tier policy.
pub struct Lookup {
    tiers: Vec<Tier>,
}

impl Lookup {
    /// Builds tiers from configuration.
    pub fn new(config: &ProxyConfig) -> Result<Self, ServeError> {
        let tiers = config.tiers.iter().map(|t| Tier::new(t, config.timeout)).collect::<Result<_, _>>()?;
        Ok(Self { tiers })
    }

    /// The configured tier with this name.
    pub fn tier(&self, name: TierName) -> Option<&Tier> {
        self.tiers.iter().find(|t| t.name == name)
    }

    /// Consults tiers in order; only a genuine miss moves to the next tier.
    pub async fn find(&self, hash: &StorePathHash) -> Result<Option<(TierName, NarInfo)>, ProxyError> {
        for tier in &self.tiers {
            if let Some(found) = tier.resolve(hash).await? {
                return Ok(Some((tier.name, found.narinfo)));
            }
        }
        Ok(None)
    }
}

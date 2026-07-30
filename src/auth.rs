//! 凭证管理: 到期预判 + 单飞刷新 + 落盘。

use std::sync::Arc;

use anyhow::{bail, Result};
use tokio::sync::Mutex;

use crate::oauth;
use crate::provider::Provider;
use crate::store::{self, Credential};

/// 提前多久刷新 (秒)。留足网络抖动与重试余量。
const REFRESH_MARGIN: u64 = 300;

pub struct AuthManager {
    http: reqwest::Client,
    anthropic: Mutex<Option<Credential>>,
    codex: Mutex<Option<Credential>>,
}

impl AuthManager {
    pub fn load(http: reqwest::Client) -> Result<Arc<Self>> {
        let store = store::load()?;
        Ok(Arc::new(Self {
            http,
            anthropic: Mutex::new(store.get(Provider::Anthropic.key()).cloned()),
            codex: Mutex::new(store.get(Provider::Codex.key()).cloned()),
        }))
    }

    fn slot(&self, p: Provider) -> &Mutex<Option<Credential>> {
        match p {
            Provider::Anthropic => &self.anthropic,
            Provider::Codex => &self.codex,
        }
    }

    pub async fn snapshot(&self, p: Provider) -> Option<Credential> {
        self.slot(p).lock().await.clone()
    }

    /// 取可用 access token。锁住整段 -> 并发请求只会触发一次刷新。
    ///
    /// `force` 用于上游返回 401 后的强制续期。
    pub async fn token(&self, p: Provider, force: bool) -> Result<Credential> {
        let mut guard = self.slot(p).lock().await;
        if guard.is_none() {
            // serve 期间才 login 的情况: 回读磁盘, 免重启。
            *guard = store::load()?.get(p.key()).cloned();
        }
        let Some(cred) = guard.clone() else {
            bail!("{p} 未登录: 先执行 `jj-agentic-proxy login {p}`");
        };
        if !force && !cred.stale(REFRESH_MARGIN) {
            return Ok(cred);
        }
        tracing::info!(provider = %p, force, "刷新 access token");
        let fresh = oauth::refresh(&self.http, p, &cred).await?;
        store::put(p, &fresh)?;
        *guard = Some(fresh.clone());
        Ok(fresh)
    }

    /// 登录成功后写盘并热更新内存。
    pub async fn set(&self, p: Provider, cred: Credential) -> Result<()> {
        store::put(p, &cred)?;
        *self.slot(p).lock().await = Some(cred);
        Ok(())
    }
}

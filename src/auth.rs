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
    /// `rejected_token` = 上游刚拒绝的 token；若并发请求已刷新，则直接复用新 token。
    pub async fn token(&self, p: Provider, rejected_token: Option<&str>) -> Result<Credential> {
        let mut guard = self.slot(p).lock().await;
        // login/logout 是独立进程；每次请求同步小文件，保证切换账号立即生效。
        *guard = store::load()?.get(p.key()).cloned();
        let Some(cred) = guard.clone() else {
            bail!("{p} 未登录: 先执行 `jj-agentic-proxy login {p}`");
        };
        let rejected_current =
            rejected_token.is_some_and(|token| token == cred.access_token.as_str());
        if !refresh_needed(&cred, rejected_token) {
            return Ok(cred);
        }
        tracing::info!(
            provider = %p,
            after_unauthorized = rejected_current,
            "刷新 access token"
        );
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

fn refresh_needed(cred: &Credential, rejected_token: Option<&str>) -> bool {
    cred.stale(REFRESH_MARGIN)
        || rejected_token.is_some_and(|token| token == cred.access_token.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(token: &str) -> Credential {
        Credential {
            access_token: token.into(),
            refresh_token: "refresh".into(),
            expires_at: u64::MAX,
            account: None,
            account_id: None,
            plan: None,
        }
    }

    #[test]
    fn unauthorized_refresh_reuses_concurrently_rotated_token() {
        let old = valid("old");
        assert!(refresh_needed(&old, Some("old")));

        let fresh = valid("fresh");
        assert!(!refresh_needed(&fresh, Some("old")));
        assert!(!refresh_needed(&fresh, None));
    }
}

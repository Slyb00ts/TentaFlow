// ============ File: coding_agent_proxy.rs — Authenticated provider egress for managed agent processes. ============

use crate::code_studio::egress::proxy::{EgressEventSink, EgressProxy};
use crate::code_studio::egress::resolver::SystemResolver;
use crate::code_studio::egress::{
    EgressEvent, EgressGateway, EgressGatewayConfig, EgressPolicy, HostPattern,
};
use crate::code_studio::models::EgressEnforcement;
use anyhow::{bail, Result};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

struct ProxyOwner {
    pid: u32,
    generation: uuid::Uuid,
    task: tokio::task::AbortHandle,
}

fn runtime_owners() -> &'static parking_lot::Mutex<HashMap<String, ProxyOwner>> {
    static OWNERS: OnceLock<parking_lot::Mutex<HashMap<String, ProxyOwner>>> = OnceLock::new();
    OWNERS.get_or_init(Default::default)
}

pub fn owns_runtime(account_id: &str, pid: u32) -> bool {
    runtime_owners()
        .lock()
        .get(account_id)
        .is_some_and(|owner| owner.pid == pid && !owner.task.is_finished())
}

struct Audit;
impl EgressEventSink for Audit {
    fn record(&self, event: EgressEvent) {
        tracing::info!(account_id = %event.workspace_id, host = %event.host,
            port = event.port, outcome = ?event.outcome, denial = ?event.denial, "agent egress");
    }
}

pub struct AgentProxy {
    account_id: String,
    task: Option<tokio::task::JoinHandle<()>>,
    port: u16,
    url: String,
}

impl AgentProxy {
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn monitor(mut self, pid: u32) {
        if let Some(task) = self.task.take() {
            let account_id = self.account_id.clone();
            let generation = uuid::Uuid::new_v4();
            runtime_owners().lock().insert(
                account_id.clone(),
                ProxyOwner {
                    pid,
                    generation,
                    task: task.abort_handle(),
                },
            );
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    #[cfg(unix)]
                    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
                    #[cfg(not(unix))]
                    let alive = {
                        let _ = pid;
                        false
                    };
                    if !alive || task.is_finished() {
                        task.abort();
                        let mut owners = runtime_owners().lock();
                        if owners
                            .get(&account_id)
                            .is_some_and(|owner| owner.generation == generation)
                        {
                            owners.remove(&account_id);
                        }
                        break;
                    }
                }
            });
        }
    }
}

impl Drop for AgentProxy {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start(engine_id: &str, account_id: &str) -> Result<AgentProxy> {
    if !cfg!(target_os = "macos") {
        bail!("native agent proxy isolation is currently supported only on macOS");
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let policy = EgressGateway::for_workspace(
        provider_config(engine_id, account_id, &token)?,
        Arc::new(SystemResolver),
    );
    let gateway = policy
        .gateway()
        .ok_or_else(|| anyhow::anyhow!("agent egress cannot be enforced"))?
        .clone();
    let proxy = EgressProxy::bind(gateway, Arc::new(Audit), "127.0.0.1:0".parse()?).await?;
    let port = proxy.local_addr()?.port();
    Ok(AgentProxy {
        account_id: account_id.into(),
        task: Some(tokio::spawn(proxy.run())),
        port,
        url: format!("http://tf:{token}@127.0.0.1:{port}"),
    })
}

fn provider_config(engine_id: &str, account_id: &str, token: &str) -> Result<EgressGatewayConfig> {
    let domains: &[&str] = match engine_id {
        "codex" => &["chatgpt.com", "auth.openai.com", "api.openai.com"],
        "claude-code" => &[
            "api.anthropic.com",
            "claude.ai",
            "platform.claude.com",
            "console.anthropic.com",
        ],
        "grok-build" => &[
            "auth.x.ai",
            "accounts.x.ai",
            "cli-chat-proxy.grok.com",
            "api.x.ai",
        ],
        "muse-code" => &["api.meta.ai", "auth.meta.com"],
        _ => bail!("provider has no verified agent egress policy"),
    };
    Ok(EgressGatewayConfig {
        workspace_id: account_id.into(),
        enforcement: EgressEnforcement::ProcessSandbox,
        policy: EgressPolicy::OrgApproved,
        workspace_allowlist: domains
            .iter()
            .map(|host| HostPattern::parse(host))
            .collect::<Result<_>>()?,
        org_approved: domains
            .iter()
            .map(|host| HostPattern::parse(host))
            .collect::<Result<_>>()?,
        local_services: vec![],
        proxy_token: token.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::egress::{EgressContext, EgressRequest, RequestKind};
    struct Resolver;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn replaced_proxy_keeps_ownership_when_old_monitor_exits() {
        let id = uuid::Uuid::new_v4().to_string();
        let pid = std::process::id();
        let first = start("codex", &id).await.unwrap();
        let first_task = first.task.as_ref().unwrap().abort_handle();
        assert!(!owns_runtime(&id, pid));
        first.monitor(pid);
        assert!(owns_runtime(&id, pid));
        assert!(!owns_runtime(&id, pid.wrapping_add(1)));

        let second = start("codex", &id).await.unwrap();
        let second_task = second.task.as_ref().unwrap().abort_handle();
        second.monitor(pid);
        first_task.abort();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(owns_runtime(&id, pid));

        second_task.abort();
        while !second_task.is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(!owns_runtime(&id, pid));
        runtime_owners().lock().remove(&id);
    }
    impl crate::code_studio::egress::resolver::Resolver for Resolver {
        fn resolve(&self, _: &str, port: u16) -> Result<Vec<std::net::SocketAddr>> {
            Ok(vec![std::net::SocketAddr::from(([93, 184, 216, 34], port))])
        }
    }

    #[test]
    fn provider_policy_allows_selected_provider_and_denies_other_destinations() {
        let policy = EgressGateway::for_workspace(
            provider_config("codex", "account", "token").unwrap(),
            Arc::new(Resolver),
        );
        let gateway = policy.gateway().unwrap();
        for (url, allowed) in [
            ("https://chatgpt.com/backend-api/codex", true),
            ("https://api.openai.com/v1", true),
            ("https://api.anthropic.com", false),
            ("http://127.0.0.1:8090", false),
            ("http://169.254.169.254", false),
            ("https://chatgpt.com:444", false),
        ] {
            let request = EgressRequest::from_url(
                EgressContext::default(),
                RequestKind::Http {
                    method: "GET".into(),
                },
                url,
            )
            .unwrap();
            assert_eq!(gateway.screen(&request).is_ok(), allowed, "{url}");
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn proxy_requires_credential_before_forwarding() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let proxy = start("codex", "fixture").await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        stream
            .write_all(b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n")
            .await
            .unwrap();
        let mut output = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            stream.read_to_end(&mut output),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .starts_with("HTTP/1.1 407"));
    }
}

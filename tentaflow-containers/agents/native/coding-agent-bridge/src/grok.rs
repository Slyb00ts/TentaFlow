// ============ File: grok.rs — Grok Build ACP sessions over the supervised JSON-RPC transport. ============

use crate::{
    process, push_event,
    rpc::{Inbound, JsonRpc, RpcClient},
    ApiError, Event,
};
use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex as SyncMutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

struct Approval {
    wire_id: Value,
    options: Vec<Value>,
}

pub(crate) struct GrokRuntime {
    pub(crate) session_id: String,
    rpc: JsonRpc,
    approvals: Arc<SyncMutex<HashMap<u64, Approval>>>,
    active_turn: Arc<AtomicBool>,
    events: Arc<SyncMutex<Vec<Event>>>,
}

fn permission_result(options: &[Value], decision: &str) -> Result<Value> {
    let kind = match decision {
        "approved" => "allow_once",
        // ACP's allow_always is broader than the bridge's session-only grant.
        "approved_for_session" => "allow_once",
        "denied" => "reject_once",
        "abort" => return Ok(json!({"outcome":{"outcome":"cancelled"}})),
        _ => return Err(anyhow!("unsupported permission decision")),
    };
    if let Some(id) = options
        .iter()
        .find(|option| option["kind"] == kind)
        .and_then(|option| option["optionId"].as_str())
    {
        return Ok(json!({"outcome":{"outcome":"selected","optionId":id}}));
    }
    if decision == "denied" {
        return Ok(json!({"outcome":{"outcome":"cancelled"}}));
    }
    Err(anyhow!("provider did not offer a one-time approval"))
}

fn initialize_params() -> Value {
    json!({"protocolVersion":1,"clientInfo":{"name":"tentaflow","version":"0.1.0"},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}})
}

impl GrokRuntime {
    async fn connect(
        workspace: &str,
        model: Option<&str>,
        env: &[(String, String)],
        args: &[String],
        events: Arc<SyncMutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<(Self, Value)> {
        if !args.is_empty() {
            return Err(anyhow!("custom Grok arguments are not supported"));
        }
        let mut argv = vec![
            "grok".into(),
            "--no-auto-update".into(),
            "agent".into(),
            "--no-leader".into(),
        ];
        if let Some(model) = model {
            argv.extend(["--model".into(), model.into()]);
        }
        argv.push("stdio".into());
        let (rpc, mut inbound) = JsonRpc::spawn(argv, workspace, env, processes, "grok-acp")?;
        let client = rpc.client();
        let approvals = Arc::new(SyncMutex::new(HashMap::new()));
        let pending = approvals.clone();
        let incoming_events = events.clone();
        let incoming_client = client.clone();
        let next_approval = AtomicU64::new(1);
        tokio::spawn(async move {
            while let Some(message) = inbound.recv().await {
                match message {
                    Inbound::Closed(reason) => {
                        pending.lock().clear();
                        push_event(
                            &incoming_events,
                            "grok",
                            json!({"method":"transport/closed","params":{"message":reason}}),
                        );
                        break;
                    }
                    Inbound::Frame(value) => {
                        if let Some(wire_id) = value.get("id") {
                            if value["method"] == "session/request_permission" {
                                let Some(options) =
                                    value.pointer("/params/options").and_then(Value::as_array)
                                else {
                                    let _ = incoming_client.write(json!({"id":wire_id,"error":{"code":-32602,"message":"permission options missing"}})).await;
                                    continue;
                                };
                                let id = next_approval.fetch_add(1, Ordering::Relaxed);
                                pending.lock().insert(
                                    id,
                                    Approval {
                                        wire_id: wire_id.clone(),
                                        options: options.clone(),
                                    },
                                );
                                push_event(
                                    &incoming_events,
                                    "approval_request",
                                    json!({"request_id":id,"method":"session/request_permission","params":value["params"]}),
                                );
                            } else {
                                let _ = incoming_client.write(json!({"id":wire_id,"error":{"code":-32601,"message":"client capability is not supported"}})).await;
                            }
                        } else {
                            push_event(&incoming_events, "grok", value);
                        }
                    }
                }
            }
        });
        let initialized = client
            .request("initialize", initialize_params(), Duration::from_secs(30))
            .await?;
        if initialized
            .pointer("/result/protocolVersion")
            .and_then(Value::as_u64)
            != Some(1)
        {
            return Err(anyhow!("Grok returned an unsupported ACP protocol version"));
        }
        Ok((
            Self {
                session_id: String::new(),
                rpc,
                approvals,
                active_turn: Arc::new(AtomicBool::new(false)),
                events,
            },
            initialized,
        ))
    }

    pub(crate) async fn discover(
        workspace: &str,
        env: &[(String, String)],
        processes: &process::Registry,
    ) -> Result<Value> {
        let (mut runtime, initialized) = Self::connect(
            workspace,
            None,
            env,
            &[],
            Arc::new(SyncMutex::new(Vec::new())),
            processes,
        )
        .await?;
        if runtime.shutdown().await == process::ProcessState::Running {
            return Err(anyhow!("Grok discovery process did not stop"));
        }
        Ok(initialized)
    }

    pub(crate) async fn spawn(
        workspace: &str,
        resume: Option<&str>,
        model: Option<&str>,
        env: &[(String, String)],
        args: &[String],
        events: Arc<SyncMutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let (mut runtime, initialized) =
            Self::connect(workspace, model, env, args, events, processes).await?;
        let params =
            json!({"cwd":workspace,"mcpServers":[],"_meta":{"yoloMode":false,"autoMode":false}});
        let response = if let Some(session_id) = resume {
            if initialized.pointer("/result/agentCapabilities/loadSession")
                != Some(&Value::Bool(true))
            {
                return Err(anyhow!("Grok does not support loading sessions"));
            }
            let mut params = params;
            params["sessionId"] = session_id.into();
            runtime
                .rpc
                .client()
                .request("session/load", params, Duration::from_secs(60))
                .await?
        } else {
            runtime
                .rpc
                .client()
                .request("session/new", params, Duration::from_secs(60))
                .await?
        };
        runtime.session_id = match resume {
            Some(id) => id.to_owned(),
            None => response
                .pointer("/result/sessionId")
                .and_then(Value::as_str)
                .context("Grok session response has no sessionId")?
                .to_owned(),
        };
        push_event(
            &runtime.events,
            "vendor_session",
            json!({"id":runtime.session_id}),
        );
        Ok(runtime)
    }

    pub(crate) async fn turn(&self, prompt: &str) -> Result<()> {
        if self.active_turn.swap(true, Ordering::AcqRel) {
            return Err(anyhow!("Grok already has an active turn"));
        }
        let client = self.rpc.client();
        let session_id = self.session_id.clone();
        let prompt = prompt.to_owned();
        let active = self.active_turn.clone();
        let events = self.events.clone();
        let approvals = self.approvals.clone();
        tokio::spawn(async move {
            let result = client
                .request(
                    "session/prompt",
                    json!({"sessionId":session_id,"prompt":[{"type":"text","text":prompt}]}),
                    Duration::from_secs(3600),
                )
                .await;
            match result {
                Ok(response) => push_event(
                    &events,
                    "grok",
                    json!({"method":"session/prompt_result","params":{"sessionId":session_id,"result":response["result"]}}),
                ),
                Err(error) => push_event(
                    &events,
                    "grok",
                    json!({"method":"session/prompt_error","params":{"sessionId":session_id,"message":error.to_string()}}),
                ),
            }
            approvals.lock().clear();
            active.store(false, Ordering::Release);
        });
        Ok(())
    }

    pub(crate) async fn answer_approval(
        &self,
        request_id: u64,
        decision: &str,
    ) -> Result<(), ApiError> {
        let approval =
            self.approvals.lock().remove(&request_id).ok_or_else(|| {
                ApiError::not_found("no Grok approval is outstanding under that id")
            })?;
        let result = match permission_result(&approval.options, decision) {
            Ok(result) => result,
            Err(error) => {
                self.approvals.lock().insert(request_id, approval);
                return Err(error.into());
            }
        };
        if let Err(error) = self
            .rpc
            .client()
            .write(json!({"id":approval.wire_id,"result":result}))
            .await
        {
            self.approvals.lock().insert(request_id, approval);
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&mut self) -> process::ProcessState {
        let outstanding: Vec<_> = self
            .approvals
            .lock()
            .drain()
            .map(|(_, value)| value)
            .collect();
        let client: RpcClient = self.rpc.client();
        for approval in outstanding {
            let _ = client
                .write(json!({"id":approval.wire_id,"result":{"outcome":{"outcome":"cancelled"}}}))
                .await;
        }
        if !self.session_id.is_empty() {
            let _ = client
                .notify("session/cancel", json!({"sessionId":self.session_id}))
                .await;
        }
        self.rpc.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    #[ignore = "requires an explicitly selected Grok binary and managed sandbox environment"]
    async fn real_grok_discovers_models_without_credentials() {
        let binary = std::env::var("TENTAFLOW_TEST_GROK_BINARY").expect("test Grok binary");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let project = root.join("project");
        let private = root.join("private");
        let bin = private.join("bin");
        for path in [
            &project,
            &bin,
            &private.join("home"),
            &private.join("tmp"),
            &private.join("grok"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::os::unix::fs::symlink(binary, bin.join("grok")).unwrap();
        let mut env = [
            ("HOME", private.join("home")),
            ("TMPDIR", private.join("tmp")),
            ("GROK_HOME", private.join("grok")),
            ("TENTAFLOW_AGENT_PRIVATE_ROOT", private.clone()),
        ]
        .into_iter()
        .map(|(key, path)| (key.to_string(), path.display().to_string()))
        .collect::<Vec<_>>();
        env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())));
        let registry = process::Registry::new(&root).unwrap();
        let result = GrokRuntime::discover(project.to_str().unwrap(), &env, &registry)
            .await
            .unwrap();
        assert_eq!(result["result"]["protocolVersion"], 1);
        assert!(
            result["result"]["_meta"]["modelState"].is_object(),
            "{result}"
        );
        assert!(!private.join("grok/auth.json").exists());
    }

    #[test]
    fn permissions_use_only_current_offered_one_time_options() {
        let options = vec![
            json!({"kind":"allow_once","optionId":"yes-2"}),
            json!({"kind":"reject_once","optionId":"no-2"}),
            json!({"kind":"allow_always","optionId":"forever"}),
        ];
        assert_eq!(
            permission_result(&options, "approved").unwrap()["outcome"]["optionId"],
            "yes-2"
        );
        assert_eq!(
            permission_result(&options, "approved_for_session").unwrap()["outcome"]["optionId"],
            "yes-2"
        );
        assert_eq!(
            permission_result(&options, "denied").unwrap()["outcome"]["optionId"],
            "no-2"
        );
        assert_eq!(
            permission_result(&options, "abort").unwrap()["outcome"]["outcome"],
            "cancelled"
        );
        assert!(permission_result(&options[2..], "approved").is_err());
    }

    #[test]
    fn filesystem_and_terminal_reverse_capabilities_are_not_advertised() {
        let params = initialize_params();
        assert_eq!(params["clientCapabilities"]["fs"]["readTextFile"], false);
        assert_eq!(params["clientCapabilities"]["terminal"], false);
    }
}

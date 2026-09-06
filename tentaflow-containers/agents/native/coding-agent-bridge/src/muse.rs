// ============ File: muse.rs — Muse MSP sessions and stage-bound approvals ============

use crate::{
    process, push_event,
    rpc::{Inbound, JsonRpc},
    ApiError, Event,
};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

const RPC_TIMEOUT: Duration = Duration::from_secs(60);

pub struct MuseRuntime {
    pub session_id: String,
    rpc: JsonRpc,
    approvals: Arc<Mutex<HashMap<u64, Value>>>,
}

impl MuseRuntime {
    async fn connect(
        workspace: &str,
        env: &[(String, String)],
        args: &[String],
        events: Arc<Mutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let mut argv = vec!["muse".into(), "serve".into()];
        argv.extend_from_slice(args);
        let (rpc, mut incoming) = JsonRpc::spawn(argv, workspace, env, processes, "muse-msp")?;
        let client = rpc.client();
        let approvals = Arc::new(Mutex::new(HashMap::new()));
        let reader_approvals = approvals.clone();
        let reader_client = client.clone();
        let reader_events = events.clone();
        tokio::spawn(async move {
            let sequence = AtomicU64::new(1);
            while let Some(inbound) = incoming.recv().await {
                let value = match inbound {
                    Inbound::Frame(value) => value,
                    Inbound::Closed(reason) => {
                        push_event(
                            &reader_events,
                            "muse",
                            json!({"method":"transport/closed","params":{"reason":reason}}),
                        );
                        break;
                    }
                };
                if let Some(id) = value.get("id") {
                    let _ = reader_client.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client method"}})).await;
                    continue;
                }
                let method = value["method"].as_str().unwrap_or_default();
                let params = &value["params"];
                match method {
                    "approval/requested" | "approval/updated" => {
                        if let Ok(id) = register_approval(&reader_approvals, &sequence, params) {
                            push_event(
                                &reader_events,
                                "approval_request",
                                json!({"request_id":id,"method":method,"params":params}),
                            );
                        } else {
                            push_event(
                                &reader_events,
                                "muse",
                                json!({"method":"protocol/error","params":{"message":"Malformed approval request; no decision sent"}}),
                            );
                        }
                    }
                    "approval/resolved" => {
                        reader_approvals
                            .lock()
                            .retain(|_, pending| pending["approvalId"] != params["approvalId"]);
                    }
                    "userInput/requested" => {
                        let request = json!({"commandId":command_id(),"sessionId":params["sessionId"],"userInputId":params["userInputId"],"reason":"This client does not provide structured user input"});
                        if let Err(error) = reader_client
                            .request("userInput/cancel", request, RPC_TIMEOUT)
                            .await
                        {
                            push_event(
                                &reader_events,
                                "muse",
                                json!({"method":"protocol/error","params":{"message":error.to_string()}}),
                            );
                        }
                    }
                    _ => {}
                }
                push_event(&reader_events, "muse", value);
            }
        });
        let runtime = Self {
            session_id: String::new(),
            rpc,
            approvals,
        };
        let initialized = client.request("initialize", json!({"clientInfo":{"name":"tentaflow","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}}), RPC_TIMEOUT).await?;
        if initialized.pointer("/result/serverInfo").is_none() {
            bail!("Muse initialize omitted serverInfo");
        }
        client.notify("initialized", json!({})).await?;
        Ok(runtime)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        workspace: &str,
        resume: Option<&str>,
        fork: bool,
        model: Option<&str>,
        env: &[(String, String)],
        args: &[String],
        events: Arc<Mutex<Vec<Event>>>,
        processes: &process::Registry,
    ) -> Result<Self> {
        let mut runtime = Self::connect(workspace, env, args, events.clone(), processes).await?;
        let client = runtime.rpc.client();
        let (method, params) = if let Some(id) = resume {
            (
                if fork {
                    "session/fork"
                } else {
                    "session/resume"
                },
                json!({"commandId":command_id(),"sessionId":id}),
            )
        } else {
            let mut params = json!({"commandId":command_id(),"workspaceRoot":workspace});
            if let Some(model) = model {
                params["modelId"] = json!(model);
            }
            ("session/start", params)
        };
        let response = client.request(method, params, RPC_TIMEOUT).await?;
        runtime.session_id = response
            .pointer("/result/session/sessionId")
            .and_then(Value::as_str)
            .context("Muse session response omitted sessionId")?
            .to_owned();
        if let Some(actual) = response
            .pointer("/result/session/workspaceRoot")
            .and_then(Value::as_str)
        {
            if std::fs::canonicalize(actual)? != std::fs::canonicalize(workspace)? {
                runtime.rpc.shutdown().await;
                bail!("Muse resumed session belongs to a different workspace");
            }
        } else {
            runtime.rpc.shutdown().await;
            bail!("Muse session did not confirm its workspace");
        }
        if resume.is_some() {
            if let Some(model) = model {
                client.request("session/setModel", json!({"commandId":command_id(),"sessionId":runtime.session_id,"model":{"modelId":model}}), RPC_TIMEOUT).await?;
            }
        }
        push_event(&events, "vendor_session", json!({"id":runtime.session_id}));
        Ok(runtime)
    }

    pub async fn discover(
        workspace: &str,
        env: &[(String, String)],
        processes: &process::Registry,
    ) -> Result<Value> {
        let mut runtime = Self::connect(
            workspace,
            env,
            &[],
            Arc::new(Mutex::new(Vec::new())),
            processes,
        )
        .await?;
        let response = runtime
            .rpc
            .client()
            .request("model/list", json!({}), RPC_TIMEOUT)
            .await;
        if runtime.shutdown().await == process::ProcessState::Running {
            bail!("Muse discovery process termination unconfirmed");
        }
        let response = response?;
        let models = response
            .pointer("/result/models")
            .and_then(Value::as_array)
            .context("Muse model/list omitted models")?;
        Ok(json!(models))
    }

    pub async fn turn(&self, prompt: &str) -> Result<()> {
        let response = self.rpc.client().request("turn/start", json!({"commandId":command_id(),"sessionId":self.session_id,"input":[{"type":"text","text":prompt}],"ifBusy":"queue"}), RPC_TIMEOUT).await?;
        response
            .pointer("/result/turnId")
            .and_then(Value::as_str)
            .context("Muse turn ack omitted turnId")?;
        Ok(())
    }

    pub async fn answer_approval(&self, request_id: u64, decision: &str) -> Result<(), ApiError> {
        let approval = self
            .approvals
            .lock()
            .get(&request_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("approval stage is no longer pending"))?;
        let params = approval_decision(&approval, decision)
            .map_err(|error| ApiError::internal(&error.to_string()))?;
        self.rpc
            .client()
            .request("approval/decide", params, RPC_TIMEOUT)
            .await?;
        self.approvals.lock().remove(&request_id);
        Ok(())
    }

    pub async fn shutdown(&mut self) -> process::ProcessState {
        self.rpc.shutdown().await
    }
}

fn command_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn register_approval(
    approvals: &Mutex<HashMap<u64, Value>>,
    sequence: &AtomicU64,
    params: &Value,
) -> Result<u64> {
    let approval_id = params["approvalId"]
        .as_str()
        .context("approvalId missing")?;
    if !params["currentRequirementId"].is_object() || !params["availableChoices"].is_array() {
        bail!("approval stage and choices required");
    }
    let mut approvals = approvals.lock();
    approvals.retain(|_, pending| pending["approvalId"].as_str() != Some(approval_id));
    let id = sequence.fetch_add(1, Ordering::Relaxed);
    approvals.insert(id, params.clone());
    Ok(id)
}

fn approval_decision(approval: &Value, decision: &str) -> Result<Value> {
    if !matches!(decision, "approved" | "denied" | "abort") {
        bail!("Muse requires a decision limited to this operation");
    }
    let choices = approval["availableChoices"]
        .as_array()
        .context("approval choices missing")?;
    let choices: Vec<_> = choices
        .iter()
        .filter(|choice| choice["decision"] == decision && choice["scope"] == "once")
        .collect();
    if choices.len() != 1 {
        bail!("Muse did not offer a unique operation-only decision");
    }
    let choice = choices[0]["choiceId"]
        .as_str()
        .context("choiceId missing")?;
    Ok(
        json!({"commandId":command_id(),"sessionId":approval["sessionId"],"approvalId":approval["approvalId"],"requirementId":approval["currentRequirementId"],"choiceId":choice}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires an explicitly selected Muse binary and managed sandbox environment"]
    async fn real_muse_session_reports_missing_auth_and_cleans_up() {
        let binary = std::env::var("TENTAFLOW_TEST_MUSE_BINARY").expect("test Muse binary");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let project = root.join("project");
        let private = root.join("private");
        let bin = private.join("bin");
        for path in [
            &project,
            &private,
            &bin,
            &private.join("home"),
            &private.join("tmp"),
            &private.join("config"),
            &private.join("data"),
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(binary, bin.join("muse")).unwrap();
        let mut env = [
            ("HOME", private.join("home")),
            ("TMPDIR", private.join("tmp")),
            ("XDG_CONFIG_HOME", private.join("config")),
            ("XDG_DATA_HOME", private.join("data")),
            ("TENTAFLOW_AGENT_PRIVATE_ROOT", private.clone()),
        ]
        .into_iter()
        .map(|(name, path)| (name.to_string(), path.display().to_string()))
        .collect::<Vec<_>>();
        env.push(("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())));
        let events = Arc::new(Mutex::new(Vec::new()));
        let registry = process::Registry::new(&root).unwrap();
        let models = MuseRuntime::discover(project.to_str().unwrap(), &env, &registry)
            .await
            .unwrap();
        assert!(models.is_array());
        let mut runtime = MuseRuntime::spawn(
            project.to_str().unwrap(),
            None,
            false,
            None,
            &env,
            &[],
            events.clone(),
            &registry,
        )
        .await
        .unwrap();
        runtime.turn("Reply with a short greeting").await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if events
                    .lock()
                    .iter()
                    .any(|event| event.data["method"] == "turn/completed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let frame = events
            .lock()
            .iter()
            .find(|event| event.data["method"] == "turn/completed")
            .unwrap()
            .data
            .clone();
        assert_eq!(frame["params"]["terminal"], "failed", "{frame}");
        assert!(frame["params"]["error"].is_object(), "{frame}");
        assert_ne!(runtime.shutdown().await, process::ProcessState::Running);
        assert!(!private.join("config/muse/auth.json").exists());
    }

    #[test]
    fn stage_update_invalidates_prior_operator_decision() {
        let pending = Mutex::new(HashMap::new());
        let sequence = AtomicU64::new(1);
        let mut frame = json!({"sessionId":"s","approvalId":"a","currentRequirementId":{"approvalId":"a","sourceIndex":1},"availableChoices":[]});
        let old = register_approval(&pending, &sequence, &frame).unwrap();
        frame["currentRequirementId"]["sourceIndex"] = json!(2);
        let new = register_approval(&pending, &sequence, &frame).unwrap();
        assert!(!pending.lock().contains_key(&old));
        assert_eq!(
            pending.lock()[&new]["currentRequirementId"]["sourceIndex"],
            2
        );
    }
    #[test]
    fn approval_cannot_accidentally_persist_a_rule() {
        let mut frame = json!({"sessionId":"s","approvalId":"a","currentRequirementId":{"approvalId":"a","sourceIndex":3},"availableChoices":[{"choiceId":"persist","decision":"approved","scope":"localPersistent"}]});
        assert!(approval_decision(&frame, "approved").is_err());
        frame["availableChoices"]
            .as_array_mut()
            .unwrap()
            .push(json!({"choiceId":"once","decision":"approved","scope":"once"}));
        let answer = approval_decision(&frame, "approved").unwrap();
        assert_eq!(answer["choiceId"], "once");
        assert_eq!(answer["requirementId"]["sourceIndex"], 3);
        assert_eq!(
            uuid::Uuid::parse_str(answer["commandId"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );
        assert!(approval_decision(&frame, "approved_for_session").is_err());
    }
}

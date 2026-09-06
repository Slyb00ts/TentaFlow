// ============ File: account_move.rs — persistent, exclusive relocation of idle agent credentials ============
use super::{coding_agent, ports::PortAllocator, transport::Transport};
use crate::{
    db::DbPool,
    mesh::{iroh_manager::IrohMeshManager, security::MeshSecurity},
    services_repo::services::{self, DeployMethod, NewService, ServiceRow, ServiceStatus},
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};

#[derive(Clone)]
pub struct MoveContext {
    pub db: DbPool,
    pub ports: Arc<PortAllocator>,
    pub mesh: Arc<IrohMeshManager>,
    pub security: Arc<MeshSecurity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub transfer_id: String,
    pub account_id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub actor_user_id: String,
    pub engine_id: String,
    pub display_name: String,
    pub grants: Vec<String>,
}
#[derive(Clone)]
struct Record {
    manifest: Manifest,
    service_id: i64,
    phase: String,
    target_service_id: Option<i64>,
    last_error: Option<String>,
    activation_complete: bool,
}
impl Record {
    fn status(&self) -> Value {
        let source = self.phase.starts_with("source_");
        json!({"transfer_id":self.manifest.transfer_id,"account_id":self.manifest.account_id,"source_node_id":self.manifest.source_node_id,"target_node_id":self.manifest.target_node_id,"direction":if source {"source"} else {"target"},"source_service_id":source.then_some(self.service_id),"target_service_id":if source {self.target_service_id} else {Some(self.service_id)},"phase":self.phase,"error":self.last_error})
    }
}
fn validate(manifest: &Manifest) -> Result<()> {
    for id in [&manifest.transfer_id, &manifest.account_id] {
        if uuid::Uuid::parse_str(id)?.to_string() != *id {
            bail!("invalid account transfer identity");
        }
    }
    for node in [&manifest.source_node_id, &manifest.target_node_id] {
        if node.len() != 64 || !node.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid transfer node identity");
        }
    }
    if manifest.source_node_id == manifest.target_node_id {
        bail!("select a different target node");
    }
    if !matches!(manifest.engine_id.as_str(), "codex" | "claude-code") {
        bail!("provider credential portability is not verified");
    }
    if manifest.display_name.is_empty()
        || manifest.display_name.len() > 512
        || manifest.grants.len() > 10000
    {
        bail!("invalid account transfer metadata");
    }
    Ok(())
}
fn admin(db: &DbPool, user: &str) -> Result<()> {
    let conn = db.read().map_err(|error| anyhow!(error.to_string()))?;
    let permitted:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id=?1 AND is_active=1 AND (is_admin=1 OR role='admin'))",[user],|row|row.get(0))?;
    if !permitted {
        bail!("administrator_required_for_account_move");
    }
    Ok(())
}
fn service(db: &DbPool, id: i64) -> Result<ServiceRow> {
    let conn = db.read().map_err(|error| anyhow!(error.to_string()))?;
    services::get(&conn, id)?.context("account service is unavailable")
}
fn load(db: &DbPool, id: &str) -> Result<Option<Record>> {
    let conn = db.read().map_err(|error| anyhow!(error.to_string()))?;
    let row:Option<(String,i64,String,Option<i64>,Option<String>,bool)>=conn.query_row("SELECT manifest_json,service_id,phase,target_service_id,last_error,activation_complete FROM coding_agent_account_moves WHERE transfer_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
    row.map(
        |(manifest, service_id, phase, target_service_id, last_error, activation_complete)| {
            Ok(Record {
                manifest: serde_json::from_str(&manifest)?,
                service_id,
                phase,
                target_service_id,
                last_error,
                activation_complete,
            })
        },
    )
    .transpose()
}
fn latest(db: &DbPool, id: i64) -> Result<Option<Record>> {
    let transfer: Option<String> = {
        let conn = db.read().map_err(|error| anyhow!(error.to_string()))?;
        conn.query_row("SELECT transfer_id FROM coding_agent_account_moves WHERE service_id=?1 ORDER BY rowid DESC LIMIT 1",[id],|row|row.get(0)).optional()?
    };
    match transfer {
        Some(id) => load(db, &id),
        None => Ok(None),
    }
}
pub fn ensure_service_mutation_allowed(db: &DbPool, service_id: i64, deleting: bool) -> Result<()> {
    {
        let conn = db.read().map_err(|error| anyhow!(error.to_string()))?;
        if let Some(row) = services::get(&conn, service_id)? {
            if row.transport == Transport::AgentRpc
                && (!row.active_deploy_id.is_empty()
                    || matches!(row.status, ServiceStatus::Deploying | ServiceStatus::Starting))
            {
                bail!("agent installation or startup is in progress; wait for it to finish");
            }
        }
    }
    let Some(record) = latest(db, service_id)? else {
        return Ok(());
    };
    if (record.phase == "target_active" && record.activation_complete)
        || (deleting && record.phase == "source_complete")
    {
        return Ok(());
    }
    bail!("account relocation owns this service; wait for the move to finish")
}
fn transition(
    db: &DbPool,
    id: &str,
    expected: &str,
    next: &str,
    target: Option<i64>,
) -> Result<()> {
    let conn = db.write().map_err(|error| anyhow!(error.to_string()))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let updated=conn.execute("UPDATE coding_agent_account_moves SET phase=?3,target_service_id=COALESCE(?4,target_service_id),last_error=NULL,updated_at=datetime('now') WHERE transfer_id=?1 AND phase=?2",params![id,expected,next,target])?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    if updated != 1 {
        bail!("account transfer state changed concurrently");
    }
    Ok(())
}
fn save_error(db: &DbPool, id: &str, error: &str) {
    if let Ok(conn) = db.write() {
        let _=conn.execute("UPDATE coding_agent_account_moves SET last_error=?2,updated_at=datetime('now') WHERE transfer_id=?1",params![id,error]);
    }
}
async fn transfer_lock(id: &str) -> Result<tokio::sync::OwnedMutexGuard<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let lock = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|error| anyhow!(error.to_string()))?
        .entry(id.to_owned())
        .or_default()
        .clone();
    Ok(lock.lock_owned().await)
}
fn private_write(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary)?;
    output.write_all(&serde_json::to_vec(value)?)?;
    output.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(path.parent().context("account state directory missing")?)?.sync_all()?;
    Ok(())
}
fn credential_path(root: &Path, engine: &str) -> Result<std::path::PathBuf> {
    match engine {
        "codex" => Ok(root.join("codex/auth.json")),
        "claude-code" => Ok(root.join("claude/setup-token.json")),
        _ => bail!("provider portability is unavailable"),
    }
}
async fn ensure_runtime(ctx: &MoveContext, row: &ServiceRow) -> Result<ServiceRow> {
    let config: Value = serde_json::from_str(&row.config_json)?;
    let account_id = config["account_id"].as_str().ok_or_else(|| anyhow!("account identity missing"))?;
    if row.runtime_pid.and_then(|pid| u32::try_from(pid).ok()).is_some_and(|pid| super::coding_agent_proxy::owns_runtime(account_id, pid))
        && coding_agent::execute(row, "runtime.status", "{}").await.is_ok()
    {
        return Ok(row.clone());
    }
    let handle = super::deploy::respawn(
        &row.engine_id,
        row.deploy_method,
        &row.config_json,
        ctx.ports.clone(),
        &ctx.db,
        ctx.security.settings_cipher(),
        row.runtime_port,
    )
    .await?;
    {
        let conn = ctx.db.write().map_err(|error| anyhow!(error.to_string()))?;
        services::update_runtime(
            &conn,
            row.id,
            handle.pid,
            handle.port,
            handle.sidecar_port,
            handle.endpoint_url.as_deref(),
        )?;
        services::update_status(&conn, row.id, ServiceStatus::Running)?;
    }
    service(&ctx.db, row.id)
}
fn persist_source(
    ctx: &MoveContext,
    row: &ServiceRow,
    manifest: &Manifest,
    phase: &str,
) -> Result<()> {
    let mut conn = ctx.db.write().map_err(|error| anyhow!(error.to_string()))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = conn.transaction()?;
    tx.execute("INSERT INTO coding_agent_account_moves(transfer_id,account_id,service_id,direction,phase,manifest_json) VALUES(?1,?2,?3,'source',?4,?5) ON CONFLICT(transfer_id) DO NOTHING",params![manifest.transfer_id,manifest.account_id,row.id,phase,serde_json::to_string(manifest)?])?;
    tx.execute("UPDATE services SET paused=1 WHERE id=?1", [row.id])?;
    tx.commit()?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}
pub async fn operate(
    ctx: MoveContext,
    service_id: i64,
    user_id: &str,
    operation: &str,
    payload: &str,
) -> Result<String> {
    let value = match operation {
        "account.move" => {
            let payload: Value = serde_json::from_str(payload)?;
            start(
                ctx,
                service_id,
                user_id,
                payload["target_node_id"]
                    .as_str()
                    .context("target_node_id is required")?,
            )
            .await?
        }
        "account.move.status" => status(ctx, service_id, user_id)?,
        _ => bail!("unsupported account move operation"),
    };
    Ok(value.to_string())
}
pub async fn start(
    ctx: MoveContext,
    service_id: i64,
    user_id: &str,
    target: &str,
) -> Result<Value> {
    admin(&ctx.db, user_id)?;
    let _account = coding_agent::lock_account(service_id)
        .await
        .map_err(|error| anyhow!(error))?;
    if let Some(record) =
        latest(&ctx.db, service_id)?.filter(|record| record.phase.starts_with("source_"))
    {
        if record.manifest.target_node_id != target {
            bail!("account already belongs to a different relocation");
        }
        launch(ctx.clone(), record.manifest.transfer_id.clone());
        return Ok(record.status());
    }
    ensure_service_mutation_allowed(&ctx.db, service_id, false)?;
    let row = service(&ctx.db, service_id)?;
    if row.transport != Transport::AgentRpc {
        bail!("service is not an agent account");
    }
    if !ctx.security.is_trusted(target) {
        bail!("target node is not trusted");
    }
    let config: Value = serde_json::from_str(&row.config_json)?;
    let root = coding_agent::account_directory(&config).map_err(|error| anyhow!(error))?;
    let marker = root.join("transfer.json");
    let manifest = if marker.exists() {
        let marker: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
        let manifest: Manifest = serde_json::from_value(marker["manifest"].clone())?;
        if manifest.target_node_id != target || manifest.source_node_id != ctx.mesh.node_id() {
            bail!("account has another transfer in progress");
        }
        manifest
    } else {
        let grants = {
            let conn = ctx.db.read().map_err(|error| anyhow!(error.to_string()))?;
            let mut statement=conn.prepare("SELECT user_id FROM coding_agent_account_grants WHERE service_id=?1 ORDER BY user_id")?;
            let rows = statement.query_map([row.id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        Manifest {
            transfer_id: uuid::Uuid::new_v4().to_string(),
            account_id: config["account_id"]
                .as_str()
                .context("account UUID missing")?
                .to_owned(),
            source_node_id: ctx.mesh.node_id(),
            target_node_id: target.to_owned(),
            actor_user_id: user_id.to_owned(),
            engine_id: row.engine_id.clone(),
            display_name: row.display_name.clone(),
            grants,
        }
    };
    validate(&manifest)?;
    remote(&ctx, target, "preflight", json!({"manifest":manifest})).await?;
    let row = ensure_runtime(&ctx, &row).await?;
    coding_agent::execute(
        &row,
        "account.transfer.freeze",
        &json!({"transfer_id":manifest.transfer_id,"manifest":manifest}).to_string(),
    )
    .await
    .map_err(|error| anyhow!(error))?;
    persist_source(&ctx, &row, &manifest, "source_frozen")?;
    let record = load(&ctx.db, &manifest.transfer_id)?.context("transfer record missing")?;
    launch(ctx, manifest.transfer_id);
    Ok(record.status())
}
pub fn status(ctx: MoveContext, service_id: i64, user_id: &str) -> Result<Value> {
    admin(&ctx.db, user_id)?;
    let Some(record) = latest(&ctx.db, service_id)? else {
        return Ok(json!({"phase":"none"}));
    };
    if matches!(record.phase.as_str(), "source_frozen" | "source_retired") {
        launch(ctx, record.manifest.transfer_id.clone());
    }
    Ok(record.status())
}
fn launch(ctx: MoveContext, id: String) {
    static RUNNING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let running = RUNNING.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let key = format!("{}:{id}", ctx.mesh.node_id());
    match running.lock() {
        Ok(mut entries) => {
            if !entries.insert(key.clone()) {
                return;
            }
        }
        Err(_) => return,
    }
    tokio::spawn(async move {
        loop {
            match drive(&ctx, &id).await {
                Ok(()) => break,
                Err(error) => {
                    save_error(&ctx.db, &id, &error.to_string());
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
        if let Ok(mut entries) = running.lock() {
            entries.remove(&key);
        }
    });
}
async fn remote(ctx: &MoveContext, target: &str, operation: &str, payload: Value) -> Result<Value> {
    if !ctx.security.is_trusted(target) {
        bail!("target node trust was revoked");
    }
    let response = ctx
        .mesh
        .send_command_and_wait(
            target,
            MeshCommandType::AgentAccountMove {
                operation: operation.to_owned(),
                payload_json: payload.to_string(),
            },
            300,
        )
        .await?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "target rejected account transfer".into())
        );
    }
    match response.payload {
        MeshCommandResponsePayload::AgentRpcResult { result_json } => {
            Ok(serde_json::from_str(&result_json)?)
        }
        _ => bail!("unexpected account transfer response"),
    }
}
#[async_trait::async_trait]
trait SourceEffects: Send + Sync {
    fn source_retired(&self, record: &Record) -> Result<bool>;
    async fn stage(&self, record: &Record) -> Result<i64>;
    async fn retire(&self, record: &Record) -> Result<()>;
    async fn activate(&self, record: &Record) -> Result<i64>;
    async fn erase_and_stop(&self, record: &Record) -> Result<()>;
}
#[async_trait::async_trait]
impl SourceEffects for MoveContext {
    fn source_retired(&self, record: &Record) -> Result<bool> {
        let row = service(&self.db, record.service_id)?;
        let config: Value = serde_json::from_str(&row.config_json)?;
        let root = coding_agent::account_directory(&config).map_err(|error| anyhow!(error))?;
        let marker: Value = serde_json::from_slice(&std::fs::read(root.join("transfer.json"))?)?;
        if marker["transfer_id"] != record.manifest.transfer_id {
            bail!("source transfer marker mismatch");
        }
        Ok(marker["phase"] == "source_retired")
    }
    async fn stage(&self, record: &Record) -> Result<i64> {
        let row = ensure_runtime(self, &service(&self.db, record.service_id)?).await?;
        let exported = coding_agent::execute(
            &row,
            "account.transfer.freeze",
            &json!({"transfer_id":record.manifest.transfer_id,"manifest":record.manifest})
                .to_string(),
        )
        .await
        .map_err(|error| anyhow!(error))?;
        let exported: Value = serde_json::from_str(&exported)?;
        let response = remote(
            self,
            &record.manifest.target_node_id,
            "stage",
            json!({"manifest":record.manifest,"credential":exported["credential"]}),
        )
        .await?;
        response["service_id"]
            .as_i64()
            .context("target omitted account service identity")
    }
    async fn retire(&self, record: &Record) -> Result<()> {
        let row = ensure_runtime(self, &service(&self.db, record.service_id)?).await?;
        coding_agent::execute(
            &row,
            "account.transfer.retire",
            &json!({"transfer_id":record.manifest.transfer_id}).to_string(),
        )
        .await
        .map_err(|error| anyhow!(error))?;
        Ok(())
    }
    async fn activate(&self, record: &Record) -> Result<i64> {
        let response = remote(
            self,
            &record.manifest.target_node_id,
            "activate",
            json!({"transfer_id":record.manifest.transfer_id}),
        )
        .await?;
        response["service_id"]
            .as_i64()
            .context("target activation omitted service identity")
    }
    async fn erase_and_stop(&self, record: &Record) -> Result<()> {
        let row = service(&self.db, record.service_id)?;
        let config: Value = serde_json::from_str(&row.config_json)?;
        let root = coding_agent::account_directory(&config).map_err(|error| anyhow!(error))?;
        match std::fs::remove_file(credential_path(&root, &row.engine_id)?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        super::deploy::stop(&row, self.ports.clone()).await?;
        let conn = self
            .db
            .write()
            .map_err(|error| anyhow!(error.to_string()))?;
        services::update_status(&conn, row.id, ServiceStatus::Stopped)?;
        Ok(())
    }
}
async fn drive(ctx: &MoveContext, id: &str) -> Result<()> {
    let record = load(&ctx.db, id)?.context("transfer record missing")?;
    if record.manifest.source_node_id != ctx.mesh.node_id() {
        bail!("this node is not the transfer source");
    }
    advance(&ctx.db, id, ctx).await
}
async fn advance(db: &DbPool, id: &str, effects: &impl SourceEffects) -> Result<()> {
    let _guard = transfer_lock(id).await?;
    let mut record = load(db, id)?.context("transfer record missing")?;
    if record.phase == "source_complete" {
        return Ok(());
    }
    if record.phase == "source_frozen" && effects.source_retired(&record)? {
        transition(db, id, "source_frozen", "source_retired", None)?;
        record = load(db, id)?.context("transfer record missing")?;
    }
    if record.phase == "source_frozen" {
        let target = effects.stage(&record).await?;
        transition(db, id, "source_frozen", "source_frozen", Some(target))?;
        effects.retire(&record).await?;
        transition(db, id, "source_frozen", "source_retired", Some(target))?;
        record = load(db, id)?.context("transfer record missing")?;
    }
    if record.phase == "source_retired" {
        let target = effects.activate(&record).await?;
        transition(db, id, "source_retired", "source_retired", Some(target))?;
        effects.erase_and_stop(&record).await?;
        transition(db, id, "source_retired", "source_complete", None)?;
        return Ok(());
    }
    bail!("source transfer phase mismatch")
}

pub async fn receive(
    ctx: &MoveContext,
    requester: &str,
    operation: &str,
    payload: &str,
) -> Result<String> {
    if !ctx.security.is_trusted(requester) {
        bail!("transfer requester is not trusted");
    }
    if payload.len() > 2 * 1024 * 1024 {
        bail!("account transfer payload exceeds limit");
    }
    let payload: Value = serde_json::from_str(payload)?;
    let result = match operation {
        "preflight" => {
            let manifest: Manifest = serde_json::from_value(payload["manifest"].clone())?;
            preflight(ctx, requester, &manifest)?;
            json!({"ready":true})
        }
        "stage" => stage(ctx, requester, &payload).await?,
        "activate" => {
            activate(
                ctx,
                requester,
                payload["transfer_id"]
                    .as_str()
                    .context("transfer id missing")?,
            )
            .await?
        }
        _ => bail!("unsupported account transfer operation"),
    };
    Ok(result.to_string())
}
fn preflight(ctx: &MoveContext, requester: &str, manifest: &Manifest) -> Result<()> {
    validate(manifest)?;
    if manifest.source_node_id != requester || manifest.target_node_id != ctx.mesh.node_id() {
        bail!("account transfer node binding mismatch");
    }
    admin(&ctx.db, &manifest.actor_user_id)?;
    if !cfg!(target_os = "macos") {
        bail!("target does not support managed provider process networking");
    }
    crate::code_studio::process_sandbox::ProcessSandbox::check_available()?;
    let conn = ctx.db.read().map_err(|error| anyhow!(error.to_string()))?;
    for user in &manifest.grants {
        if !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id=?1)",
            [user],
            |row| row.get::<_, bool>(0),
        )? {
            bail!("account users have not synchronized to the target node");
        }
    }
    Ok(())
}
async fn stage(ctx: &MoveContext, requester: &str, payload: &Value) -> Result<Value> {
    let manifest: Manifest = serde_json::from_value(payload["manifest"].clone())?;
    preflight(ctx, requester, &manifest)?;
    let _guard = transfer_lock(&manifest.transfer_id).await?;
    let config = json!({"account_id":manifest.account_id});
    let record = if let Some(record) = load(&ctx.db, &manifest.transfer_id)? {
        if record.manifest != manifest {
            bail!("transfer id was reused with different metadata");
        }
        record
    } else {
        let mut conn = ctx.db.write().map_err(|error| anyhow!(error.to_string()))?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction()?;
        let existing:Option<i64>=tx.query_row("SELECT id FROM services WHERE json_valid(config_json) AND json_extract(config_json,'$.account_id')=?1",[&manifest.account_id],|row|row.get(0)).optional()?;
        if let Some(id) = existing {
            let phase:Option<String>=tx.query_row("SELECT phase FROM coding_agent_account_moves WHERE service_id=?1 ORDER BY rowid DESC LIMIT 1",[id],|row|row.get(0)).optional()?;
            if phase.as_deref() != Some("source_complete") {
                bail!("this account UUID already exists on the target node");
            }
        }
        for user in &manifest.grants {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id=?1)",
                [user],
                |row| row.get(0),
            )?;
            if !exists {
                bail!("account users have not synchronized to the target node");
            }
        }
        let mut new = NewService::minimal(
            &manifest.engine_id,
            DeployMethod::NativeManagedCli,
            Transport::AgentRpc,
        );
        new.display_name = manifest.display_name.clone();
        new.config_json = config.to_string();
        new.paused = true;
        new.status = ServiceStatus::Starting;
        let service_id = if let Some(id) = existing {
            tx.execute("UPDATE services SET display_name=?2,config_json=?3,paused=1,status='starting' WHERE id=?1",params![id,manifest.display_name,config.to_string()])?;
            tx.execute(
                "DELETE FROM coding_agent_account_grants WHERE service_id=?1",
                [id],
            )?;
            id
        } else {
            services::insert_in_tx(&tx, &new)?
        };
        tx.execute("INSERT INTO coding_agent_account_moves(transfer_id,account_id,service_id,direction,phase,manifest_json) VALUES(?1,?2,?3,'target','target_staged',?4)",params![manifest.transfer_id,manifest.account_id,service_id,serde_json::to_string(&manifest)?])?;
        for user in &manifest.grants {
            tx.execute("INSERT INTO coding_agent_account_grants(service_id,user_id,granted_by) VALUES(?1,?2,?3)",params![service_id,user,manifest.actor_user_id])?;
        }
        tx.commit()?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        drop(conn);
        load(&ctx.db, &manifest.transfer_id)?.context("target transfer record missing")?
    };
    let _account_guard = coding_agent::lock_account(record.service_id)
        .await
        .map_err(|error| anyhow!(error))?;
    require_latest(&ctx.db, &record)?;
    if record.phase == "target_active" {
        return Ok(json!({"service_id":record.service_id,"phase":record.phase}));
    }
    if record.phase != "target_staged" {
        bail!("target transfer phase mismatch");
    }
    let credential = payload
        .get("credential")
        .context("credential material missing")?;
    if !credential
        .as_object()
        .is_some_and(|object| !object.is_empty())
    {
        bail!("invalid portable credential");
    }
    let root = coding_agent::prepare_account_directory(&config).map_err(|error| anyhow!(error))?;
    private_write(
        &root.join("transfer.json"),
        &json!({"transfer_id":manifest.transfer_id,"phase":"target_staged","manifest":manifest}),
    )?;
    private_write(&credential_path(&root, &manifest.engine_id)?, credential)?;
    ensure_runtime(ctx, &service(&ctx.db, record.service_id)?).await?;
    Ok(json!({"service_id":record.service_id,"phase":"target_staged"}))
}
fn require_latest(db: &DbPool, record: &Record) -> Result<()> {
    if latest(db, record.service_id)?
        .as_ref()
        .map(|r| &r.manifest.transfer_id)
        != Some(&record.manifest.transfer_id)
    {
        bail!("account transfer was superseded");
    }
    Ok(())
}
fn complete_activation(db: &DbPool, record: &Record) -> Result<()> {
    require_latest(db, record)?;
    if record.activation_complete {
        return Ok(());
    }
    let mut conn = db.write().map_err(|error| anyhow!(error.to_string()))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = conn.transaction()?;
    let changed=tx.execute("UPDATE coding_agent_account_moves SET activation_complete=1 WHERE transfer_id=?1 AND phase='target_active' AND activation_complete=0",[&record.manifest.transfer_id])?;
    if changed == 1 {
        services::set_paused(&tx, record.service_id, false)?;
    }
    tx.commit()?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}
async fn activate(ctx: &MoveContext, requester: &str, id: &str) -> Result<Value> {
    if !ctx.security.is_trusted(requester) {
        bail!("transfer source trust was revoked");
    }
    let _guard = transfer_lock(id).await?;
    let record = load(&ctx.db, id)?.context("target transfer is not staged")?;
    if record.manifest.source_node_id != requester
        || record.manifest.target_node_id != ctx.mesh.node_id()
    {
        bail!("transfer source binding mismatch");
    }
    if !matches!(record.phase.as_str(), "target_staged" | "target_active") {
        bail!("target transfer phase mismatch");
    }
    admin(&ctx.db, &record.manifest.actor_user_id)?;
    let _account_guard = coding_agent::lock_account(record.service_id)
        .await
        .map_err(|error| anyhow!(error))?;
    require_latest(&ctx.db, &record)?;
    if record.phase == "target_active" {
        let row = service(&ctx.db, record.service_id)?;
        let config: Value = serde_json::from_str(&row.config_json)?;
        let root = coding_agent::account_directory(&config).map_err(|error| anyhow!(error))?;
        if !root.join("transfer.json").exists() {
            if !record.activation_complete {
                ensure_runtime(ctx, &row).await?;
            }
            complete_activation(&ctx.db, &record)?;
            return Ok(json!({"service_id":row.id,"phase":"target_active"}));
        }
    }
    let row = ensure_runtime(ctx, &service(&ctx.db, record.service_id)?).await?;
    if record.phase == "target_staged" {
        transition(&ctx.db, id, "target_staged", "target_active", None)?;
    }
    coding_agent::execute(
        &row,
        "account.transfer.activate",
        &json!({"transfer_id":id}).to_string(),
    )
    .await
    .map_err(|error| anyhow!(error))?;
    complete_activation(&ctx.db, &record)?;
    Ok(json!({"service_id":row.id,"phase":"target_active"}))
}

pub async fn recover(ctx: MoveContext) -> Result<()> {
    let rows = {
        let conn = ctx.db.read().map_err(|error| anyhow!(error.to_string()))?;
        services::list_all(&conn)?
    };
    for row in rows {
        if !matches!(row.engine_id.as_str(), "codex" | "claude-code") {
            continue;
        }
        let Ok(config) = serde_json::from_str::<Value>(&row.config_json) else {
            continue;
        };
        let Ok(root) = coding_agent::account_directory(&config) else {
            continue;
        };
        let marker = root.join("transfer.json");
        if !marker.exists() {
            continue;
        }
        let result: Result<()> = async {
            let marker: Value = serde_json::from_slice(&std::fs::read(marker)?)?;
            let manifest: Manifest = serde_json::from_value(marker["manifest"].clone())?;
            validate(&manifest)?;
            if manifest.source_node_id == ctx.mesh.node_id() {
                let phase = match marker["phase"].as_str() {
                    Some("source_frozen") => "source_frozen",
                    Some("source_retired") => "source_retired",
                    _ => return Ok(()),
                };
                persist_source(&ctx, &row, &manifest, phase)?;
                if let Some(record) = load(&ctx.db, &manifest.transfer_id)? {
                    if record.phase != "source_complete" {
                        launch(ctx.clone(), manifest.transfer_id);
                    }
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(service_id=row.id, error=%error, "Account relocation recovery failed");
        }
    }
    let staged_active = {
        let conn = ctx.db.read().map_err(|error| anyhow!(error.to_string()))?;
        let mut statement = conn.prepare(
            "SELECT m.transfer_id FROM coding_agent_account_moves m WHERE m.phase='target_active' AND m.rowid=(SELECT MAX(n.rowid) FROM coding_agent_account_moves n WHERE n.service_id=m.service_id)",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in staged_active {
        let record = load(&ctx.db, &id)?.context("target transfer missing")?;
        if let Err(error) = activate(&ctx, &record.manifest.source_node_id, &id).await {
            save_error(&ctx.db, &id, &error.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct EffectState {
        retired: bool,
        activated: bool,
        erased: bool,
        stages: usize,
        fail: Option<&'static str>,
    }
    struct Effects {
        db: DbPool,
        state: Mutex<EffectState>,
    }
    #[async_trait::async_trait]
    impl SourceEffects for Effects {
        fn source_retired(&self, _: &Record) -> Result<bool> {
            Ok(self.state.lock().unwrap().retired)
        }
        async fn stage(&self, _: &Record) -> Result<i64> {
            let mut state = self.state.lock().unwrap();
            state.stages += 1;
            if state.fail == Some("stage") {
                state.fail = None;
                bail!("stage connection lost");
            }
            Ok(42)
        }
        async fn retire(&self, _: &Record) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            state.retired = true;
            if state.fail == Some("retire") {
                state.fail = None;
                bail!("retirement acknowledgement lost");
            }
            Ok(())
        }
        async fn activate(&self, record: &Record) -> Result<i64> {
            assert_eq!(
                load(&self.db, &record.manifest.transfer_id)?.unwrap().phase,
                "source_retired"
            );
            let mut state = self.state.lock().unwrap();
            assert!(state.retired);
            state.activated = true;
            if state.fail == Some("activate") {
                state.fail = None;
                bail!("activation acknowledgement lost");
            }
            Ok(42)
        }
        async fn erase_and_stop(&self, _: &Record) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            assert!(state.retired && state.activated);
            state.erased = true;
            if state.fail == Some("cleanup") {
                state.fail = None;
                bail!("runtime shutdown interrupted");
            }
            Ok(())
        }
    }
    fn fixture() -> (DbPool, String) {
        let db = crate::db::init(Path::new(":memory:")).unwrap();
        let manifest = Manifest {
            transfer_id: uuid::Uuid::new_v4().to_string(),
            account_id: uuid::Uuid::new_v4().to_string(),
            source_node_id: "a".repeat(64),
            target_node_id: "b".repeat(64),
            actor_user_id: "actor".into(),
            engine_id: "codex".into(),
            display_name: "account".into(),
            grants: vec![],
        };
        db.write().unwrap().execute("INSERT INTO coding_agent_account_moves(transfer_id,account_id,service_id,direction,phase,manifest_json) VALUES(?1,?2,7,'source','source_frozen',?3)",params![manifest.transfer_id,manifest.account_id,serde_json::to_string(&manifest).unwrap()]).unwrap();
        (db, manifest.transfer_id)
    }
    #[tokio::test]
    async fn interrupted_source_move_retries_without_two_active_copies() {
        for failure in ["stage", "retire", "activate", "cleanup"] {
            let (db, id) = fixture();
            let effects = Effects {
                db: db.clone(),
                state: Mutex::new(EffectState {
                    fail: Some(failure),
                    ..Default::default()
                }),
            };
            assert!(advance(&db, &id, &effects).await.is_err());
            let interrupted = load(&db, &id).unwrap().unwrap();
            assert_ne!(interrupted.phase, "source_complete");
            if failure == "activate" {
                assert!(!effects.state.lock().unwrap().erased);
            }
            advance(&db, &id, &effects).await.unwrap();
            let completed = load(&db, &id).unwrap().unwrap();
            assert_eq!(completed.phase, "source_complete");
            assert_eq!(completed.target_service_id, Some(42));
            let stages = effects.state.lock().unwrap().stages;
            advance(&db, &id, &effects).await.unwrap();
            let state = effects.state.lock().unwrap();
            assert!(state.retired && state.activated && state.erased);
            assert_eq!(state.stages, stages);
            assert_eq!(stages, if failure == "stage" { 2 } else { 1 });
        }
    }
    #[test]
    fn superseded_target_cannot_be_reactivated() {
        let (db, id) = fixture();
        let old = load(&db, &id).unwrap().unwrap();
        let mut onward = old.manifest.clone();
        onward.transfer_id = uuid::Uuid::new_v4().to_string();
        db.write().unwrap().execute("INSERT INTO coding_agent_account_moves(transfer_id,account_id,service_id,direction,phase,manifest_json) VALUES(?1,?2,7,'source','source_retired',?3)",params![onward.transfer_id,onward.account_id,serde_json::to_string(&onward).unwrap()]).unwrap();
        assert!(require_latest(&db, &old).is_err());
        assert!(complete_activation(&db, &old).is_err());
    }
    #[test]
    fn installation_fences_mutations_before_a_runtime_exists() {
        let (db, _) = fixture();
        let mut new =
            NewService::minimal("codex", DeployMethod::NativeManagedCli, Transport::AgentRpc);
        new.status = ServiceStatus::Deploying;
        new.active_deploy_id = uuid::Uuid::new_v4().to_string();
        let id = services::insert(&db.write().unwrap(), &new).unwrap();
        assert!(ensure_service_mutation_allowed(&db, id, false).is_err());
        assert!(ensure_service_mutation_allowed(&db, id, true).is_err());
        services::mark_deploy_failed(
            &db.write().unwrap(),
            id,
            &new.active_deploy_id,
            ServiceStatus::Failed,
            Some("installation failed"),
        )
        .unwrap();
        assert!(ensure_service_mutation_allowed(&db, id, false).is_ok());
        services::set_status(&db.write().unwrap(), id, ServiceStatus::Starting).unwrap();
        assert!(ensure_service_mutation_allowed(&db, id, true).is_err());
        services::set_status(&db.write().unwrap(), id, ServiceStatus::Stopped).unwrap();
        assert!(ensure_service_mutation_allowed(&db, id, true).is_ok());
        {
            let conn = db.write().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            services::begin_redeploy_in_tx(&tx, id, "failed-redeploy", &new.config_json).unwrap();
            tx.commit().unwrap();
        }
        assert!(ensure_service_mutation_allowed(&db, id, true).is_err());
        services::mark_failed_clear_runtime(&db.write().unwrap(), id, "installation failed").unwrap();
        assert!(ensure_service_mutation_allowed(&db, id, true).is_ok());
    }
    #[test]
    fn target_activation_recovers_pause_once_without_overriding_later_admin_pause() {
        let (db, id) = fixture();
        let mut new =
            NewService::minimal("codex", DeployMethod::NativeManagedCli, Transport::AgentRpc);
        new.paused = true;
        let service_id = {
            let conn = db.write().unwrap();
            services::insert(&conn, &new).unwrap()
        };
        db.write().unwrap().execute("UPDATE coding_agent_account_moves SET service_id=?2,direction='target',phase='target_active' WHERE transfer_id=?1",params![id,service_id]).unwrap();
        complete_activation(&db, &load(&db, &id).unwrap().unwrap()).unwrap();
        assert!(!service(&db, service_id).unwrap().paused);
        services::set_paused(&db.write().unwrap(), service_id, true).unwrap();
        complete_activation(&db, &load(&db, &id).unwrap().unwrap()).unwrap();
        assert!(service(&db, service_id).unwrap().paused);
    }
}

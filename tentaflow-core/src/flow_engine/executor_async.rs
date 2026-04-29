// =============================================================================
// Plik: flow_engine/executor_async.rs
// Opis: Asynchroniczny executor flow DAG - parsuje definicje, sortuje
//       topologicznie i wykonuje wezly przez AdapterRegistry. Zastepuje
//       synchroniczny FlowEngine dla wezlow serwisowych (LLM, RAG, STT itd.).
// =============================================================================

use super::adapters::{AdapterChunkStream, AdapterRegistry};
use super::types::{
    FlowContext, FlowDefinition, FlowEdge, FlowExecutionResult, FlowNode, FlowStepLog,
};
use crate::db::repository;
use crate::db::DbPool;
use anyhow::{bail, Result};
use chrono::Utc;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, info, warn};

const MAX_FLOW_NODES: usize = 256;
const MAX_FLOW_EDGES: usize = 1024;

/// Wstepnie sparsowany flow — wynik `serde_json::from_str` + topological_sort
/// + budowa map adjacencji. Trzymamy w cache zeby chat completion nie placil
/// tej pracy per-request (wczesniej O(N+E) na kazde wykonanie flow).
pub struct ParsedFlow {
    pub definition: Arc<FlowDefinition>,
    /// Kolejnosc topologiczna jako indeksy do `definition.nodes`.
    execution_order: Vec<usize>,
    /// Pozycja node'a w `execution_order` po jego id (uzywane przy ewaluacji
    /// blocked_edges — kodowanie krawedzi to from_pos*N + to_pos).
    node_pos_by_id: HashMap<String, usize>,
    /// Per-pozycja w execution_order: indeksy krawedzi wychodzacych z tego node'a
    /// (indeksy do `definition.edges`).
    outgoing_edges_per_pos: Vec<Vec<usize>>,
    /// Per-pozycja w execution_order: indeksy krawedzi wchodzacych.
    incoming_edges_per_pos: Vec<Vec<usize>>,
}

impl ParsedFlow {
    /// Parsuje flow_json + waliduje + sortuje topologicznie + buduje mapy adjacencji.
    pub fn parse(flow_json: &str) -> Result<Self> {
        let definition = FlowExecutorAsync::parse_flow(flow_json)?;
        let order_ids = FlowExecutorAsync::topological_sort(&definition)?;

        // node_id -> indeks w `definition.nodes` (potrzebny do mapowania order_ids -> indeksow).
        let node_idx_in_def: HashMap<&str, usize> = definition
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();

        let execution_order: Vec<usize> = order_ids
            .iter()
            .map(|id| {
                node_idx_in_def
                    .get(id.as_str())
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("Wezel '{}' nie znaleziony w definition", id))
            })
            .collect::<Result<Vec<_>>>()?;

        let node_pos_by_id: HashMap<String, usize> = order_ids
            .iter()
            .enumerate()
            .map(|(pos, id)| (id.clone(), pos))
            .collect();

        let n = execution_order.len();
        let mut outgoing_edges_per_pos: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut incoming_edges_per_pos: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (edge_idx, edge) in definition.edges.iter().enumerate() {
            if let Some(&from_pos) = node_pos_by_id.get(edge.from.as_str()) {
                outgoing_edges_per_pos[from_pos].push(edge_idx);
            }
            if let Some(&to_pos) = node_pos_by_id.get(edge.to.as_str()) {
                incoming_edges_per_pos[to_pos].push(edge_idx);
            }
        }

        Ok(Self {
            definition: Arc::new(definition),
            execution_order,
            node_pos_by_id,
            outgoing_edges_per_pos,
            incoming_edges_per_pos,
        })
    }
}

/// Asynchroniczny executor flow DAG z prawdziwymi adapterami serwisow
pub struct FlowExecutorAsync {
    db: DbPool,
    registry: Arc<AdapterRegistry>,
}

impl FlowExecutorAsync {
    pub fn new(db: DbPool, registry: Arc<AdapterRegistry>) -> Self {
        Self { db, registry }
    }

    /// Parsuje flow_json (string) na strukture FlowDefinition
    pub(super) fn parse_flow(flow_json: &str) -> Result<FlowDefinition> {
        let definition: FlowDefinition = serde_json::from_str(flow_json)?;

        if definition.nodes.is_empty() {
            bail!("Flow nie zawiera zadnych wezlow");
        }

        if definition.nodes.len() > MAX_FLOW_NODES {
            bail!(
                "Flow przekracza limit {} wezlow (ma {})",
                MAX_FLOW_NODES,
                definition.nodes.len()
            );
        }
        if definition.edges.len() > MAX_FLOW_EDGES {
            bail!(
                "Flow przekracza limit {} krawedzi (ma {})",
                MAX_FLOW_EDGES,
                definition.edges.len()
            );
        }

        let node_ids: HashSet<&str> = definition.nodes.iter().map(|n| n.id.as_str()).collect();
        for edge in &definition.edges {
            if !node_ids.contains(edge.from.as_str()) {
                bail!(
                    "Krawedz wskazuje na nieistniejacy wezel zrodlowy: {}",
                    edge.from
                );
            }
            if !node_ids.contains(edge.to.as_str()) {
                bail!(
                    "Krawedz wskazuje na nieistniejacy wezel docelowy: {}",
                    edge.to
                );
            }
        }

        Ok(definition)
    }

    /// Sortowanie topologiczne wezlow DAG (algorytm Kahna)
    pub(super) fn topological_sort(definition: &FlowDefinition) -> Result<Vec<String>> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

        for node in &definition.nodes {
            in_degree.entry(node.id.as_str()).or_insert(0);
            adjacency.entry(node.id.as_str()).or_default();
        }

        for edge in &definition.edges {
            adjacency
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
            *in_degree.entry(edge.to.as_str()).or_insert(0) += 1;
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&node, _)| node)
            .collect();

        let mut sorted: Vec<String> = Vec::with_capacity(definition.nodes.len());

        while let Some(node) = queue.pop_front() {
            sorted.push(node.to_string());

            if let Some(neighbors) = adjacency.get(node) {
                for &neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(neighbor);
                        }
                    }
                }
            }
        }

        if sorted.len() != definition.nodes.len() {
            bail!(
                "Flow zawiera cykl - posortowano {} z {} wezlow",
                sorted.len(),
                definition.nodes.len()
            );
        }

        Ok(sorted)
    }

    /// Wykonuje flow od poczatku do konca (async) na pre-sparsowanej strukturze.
    /// Tworzy rekord execution w DB, przetwarza wezly wg porzadku topologicznego
    /// delegujac serwisowe wezly do adapterow, aktualizuje rekord po zakonczeniu.
    pub async fn execute(
        &self,
        flow: &crate::db::models::DbFlow,
        parsed: &ParsedFlow,
        context: &mut FlowContext,
    ) -> Result<FlowExecutionResult> {
        let start_time = std::time::Instant::now();

        let db_clone = self.db.clone();
        let flow_id = flow.id;
        let req_id = context.request_id.clone();
        let model = context.model.clone();
        let execution_id = tokio::task::spawn_blocking(move || {
            repository::create_flow_execution(
                &db_clone,
                flow_id,
                Some(&req_id),
                Some(&model),
                "running",
            )
        })
        .await??;

        info!(
            flow_id = flow.id,
            flow_name = %flow.name,
            execution_id = execution_id,
            request_id = %context.request_id,
            "Rozpoczynam async wykonanie flow"
        );

        let definition: &FlowDefinition = &parsed.definition;
        let nodes: &[FlowNode] = &definition.nodes;
        let edges: &[FlowEdge] = &definition.edges;
        let execution_order = &parsed.execution_order;
        let n = execution_order.len();

        let mut blocked_edges: HashSet<usize> = HashSet::new();
        let mut executed_positions: HashSet<usize> = HashSet::new();
        let mut final_output = serde_json::Value::Null;
        let mut total_prompt_tokens: i64 = 0;
        let mut total_completion_tokens: i64 = 0;

        for current_pos in 0..n {
            let node_idx = execution_order[current_pos];
            let node: &FlowNode = &nodes[node_idx];
            let node_id: &str = node.id.as_str();

            // Lazy evaluation: wezel wykonuje sie jesli CO NAJMNIEJ JEDNA krawedz wejsciowa jest aktywna
            let incoming = &parsed.incoming_edges_per_pos[current_pos];
            if !incoming.is_empty() {
                let has_active_input = incoming.iter().any(|&edge_idx| {
                    let edge = &edges[edge_idx];
                    let from_pos = parsed
                        .node_pos_by_id
                        .get(edge.from.as_str())
                        .copied()
                        .unwrap_or(0);
                    !blocked_edges.contains(&(from_pos * n + current_pos))
                        && executed_positions.contains(&from_pos)
                });
                if !has_active_input {
                    debug!(node_id = %node_id, "Pomijam wezel (wszystkie wejscia zablokowane)");
                    continue;
                }
            }

            let step_start = Utc::now().to_rfc3339();
            let mut step_status = "completed".to_string();

            let result = self.execute_node(node, context).await;
            let step_output;

            match result {
                Ok(output) => {
                    let preview = output.to_string();
                    step_output = Some(truncate_utf8(&preview, 200));

                    if let Some(tokens) = output.get("tokens") {
                        let prompt_t = tokens.get("prompt").and_then(|v| v.as_i64()).unwrap_or(0);
                        let compl_t = tokens
                            .get("completion")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        total_prompt_tokens += prompt_t;
                        total_completion_tokens += compl_t;
                    }

                    let is_final = matches!(
                        node.node_type.as_str(),
                        "output"
                            | "end"
                            | "http_response"
                            | "quic_response"
                            | "stream_chunk"
                            | "router"
                    );
                    let is_condition = node.node_type == "condition" || node.node_type == "switch";

                    context.node_results.insert(node_id.to_string(), output);

                    executed_positions.insert(current_pos);

                    if is_final {
                        final_output = context
                            .node_results
                            .get(node_id)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                    }

                    if is_condition {
                        let cond_result = context.node_results.get(node_id).unwrap();
                        Self::evaluate_condition_edges(
                            current_pos,
                            cond_result,
                            parsed,
                            edges,
                            n,
                            &mut blocked_edges,
                        );
                    }
                }
                Err(e) => {
                    step_status = "error".to_string();
                    step_output = Some(format!("{}", e));
                    warn!(
                        node_id = %node_id,
                        node_type = %node.node_type,
                        error = %e,
                        "Blad wykonania wezla"
                    );

                    context.execution_log.push(FlowStepLog {
                        node_id: node_id.to_string(),
                        node_type: node.node_type.clone(),
                        started_at: step_start,
                        finished_at: Some(Utc::now().to_rfc3339()),
                        status: step_status,
                        output_preview: step_output,
                    });

                    if !context.continue_on_error {
                        break;
                    }
                    continue;
                }
            }

            context.execution_log.push(FlowStepLog {
                node_id: node_id.to_string(),
                node_type: node.node_type.clone(),
                started_at: step_start,
                finished_at: Some(Utc::now().to_rfc3339()),
                status: step_status,
                output_preview: step_output,
            });
        }

        let latency_ms = start_time.elapsed().as_millis() as i64;
        let total_tokens = total_prompt_tokens + total_completion_tokens;

        let has_errors = context.execution_log.iter().any(|s| s.status == "error");
        let final_status = if has_errors { "error" } else { "completed" };

        let execution_result = FlowExecutionResult {
            status: final_status.to_string(),
            output: final_output,
            execution_log: std::mem::take(&mut context.execution_log),
            total_latency_ms: latency_ms,
            total_tokens,
            prompt_tokens: total_prompt_tokens,
            completion_tokens: total_completion_tokens,
        };

        let log_json = serde_json::to_string(&execution_result.execution_log)?;
        let db_clone = self.db.clone();
        let final_status_owned = final_status.to_string();
        tokio::task::spawn_blocking(move || {
            repository::update_flow_execution(
                &db_clone,
                execution_id,
                &final_status_owned,
                Some(&log_json),
                Some(latency_ms),
                Some(total_tokens),
            )
        })
        .await??;

        info!(
            flow_id = flow.id,
            execution_id = execution_id,
            status = final_status,
            latency_ms = latency_ms,
            total_tokens = total_tokens,
            "Zakonczono async wykonanie flow"
        );

        Ok(execution_result)
    }

    /// Wykonuje flow w trybie streaming. Wymaga ze dokladnie jeden node w flow
    /// jest producentem streamu (ma wychodzaca edge z `from_port="stream"`),
    /// a jego adapter zaimplementuje `execute_streaming`. Nody przed producentem
    /// (topologicznie) wykonuja sie blocking; nody za producentem na stream-path
    /// musza byc pass-through (`output`/`end`/`stream_chunk`) — bo aktualnie
    /// executor nie transformuje chunków per-node na stream.
    ///
    /// Zwraca strumien ChatCompletionChunk gotowy do wyslania do klienta SSE.
    pub async fn execute_streaming_flow(
        &self,
        flow: &crate::db::models::DbFlow,
        parsed: &ParsedFlow,
        context: &mut FlowContext,
    ) -> Result<AdapterChunkStream> {
        let definition: &FlowDefinition = &parsed.definition;

        let node_map: HashMap<&str, &FlowNode> = definition
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n))
            .collect();

        let outgoing_edges: HashMap<&str, Vec<&FlowEdge>> = {
            let mut map: HashMap<&str, Vec<&FlowEdge>> = HashMap::new();
            for edge in &definition.edges {
                map.entry(edge.from.as_str()).or_default().push(edge);
            }
            map
        };

        // Znajdz unikalnego producenta streamu — node ktory ma choc jedna wychodzaca
        // edge from_port="stream". W tym S4b-cut wspieramy dokladnie jeden.
        let mut stream_producers: Vec<&str> = Vec::new();
        for node in &definition.nodes {
            if let Some(edges) = outgoing_edges.get(node.id.as_str()) {
                if edges.iter().any(|e| e.from_port == "stream") {
                    stream_producers.push(node.id.as_str());
                }
            }
        }
        if stream_producers.is_empty() {
            bail!("Flow nie zawiera edge z portem 'stream' — uzyj execute()");
        }
        if stream_producers.len() > 1 {
            bail!(
                "Flow ma wiecej niz jednego producenta streamu ({}) — multi-stream nie wspierany",
                stream_producers.len()
            );
        }
        let producer_id = stream_producers[0].to_string();

        // Nody za producentem na stream path musza byc pass-through
        let producer_outgoing = outgoing_edges
            .get(producer_id.as_str())
            .cloned()
            .unwrap_or_default();
        for edge in &producer_outgoing {
            if edge.from_port != "stream" {
                continue;
            }
            let target = node_map
                .get(edge.to.as_str())
                .ok_or_else(|| anyhow::anyhow!("stream edge wskazuje brakujacy node"))?;
            if !is_stream_passthrough(&target.node_type) {
                bail!(
                    "Node '{}' typu '{}' na stream path nie jest pass-through — \
                    streaming transformations nie sa wspierane w tym cut (S4b)",
                    target.id,
                    target.node_type
                );
            }
        }

        info!(
            flow_id = flow.id,
            flow_name = %flow.name,
            producer = %producer_id,
            "Streaming flow start"
        );

        // Pre-fix: wykonuj nody w kolejnosci topologicznej az dojdziemy do producenta.
        // Producent sam sie NIE wykonuje blocking — wywolamy execute_streaming.
        for &node_idx in &parsed.execution_order {
            let node = &definition.nodes[node_idx];
            let node_id = node.id.as_str();
            if node_id == producer_id {
                break;
            }
            let step_start = Utc::now().to_rfc3339();
            let result = self.execute_node(node, context).await;
            match result {
                Ok(output) => {
                    context.node_results.insert(node_id.to_string(), output);
                    context.execution_log.push(FlowStepLog {
                        node_id: node_id.to_string(),
                        node_type: node.node_type.clone(),
                        started_at: step_start,
                        finished_at: Some(Utc::now().to_rfc3339()),
                        status: "completed".to_string(),
                        output_preview: None,
                    });
                }
                Err(e) => {
                    bail!("pre-stream node '{}' failed: {}", node_id, e);
                }
            }
        }

        // Wywolaj producenta w trybie streaming
        let producer_node = node_map
            .get(producer_id.as_str())
            .ok_or_else(|| anyhow::anyhow!("producer node zniknal z mapy"))?;
        let adapter = self.registry.get(&producer_node.node_type).ok_or_else(|| {
            anyhow::anyhow!(
                "Brak adaptera dla typu '{}' (producent streamu)",
                producer_node.node_type
            )
        })?;

        let stream_opt = adapter
            .execute_streaming_dyn(&producer_node.config, context)
            .await;

        match stream_opt {
            Some(Ok(stream)) => Ok(stream),
            Some(Err(e)) => Err(e),
            None => bail!(
                "Adapter '{}' nie wspiera streamingu — walidacja flow powinna to zlapac",
                producer_node.node_type
            ),
        }
    }

    /// Wykonuje pojedynczy wezel - typy wewnetrzne (trigger, condition, template,
    /// output, router, pii_filter, tts_clean) obsluguje bezposrednio,
    /// typy serwisowe (llm, rag, stt, tts, embeddings, memory) deleguje do adapterow.
    async fn execute_node(
        &self,
        node: &FlowNode,
        context: &mut FlowContext,
    ) -> Result<serde_json::Value> {
        match node.node_type.as_str() {
            "trigger" | "start" | "http_request" | "quic_request" | "webhook" => {
                self.execute_trigger(node, context)
            }
            "output" | "end" | "http_response" | "quic_response" | "stream_chunk" => {
                self.execute_passthrough(node, context, "flow_output")
            }
            "router" => self.execute_passthrough(node, context, "router_output"),
            "condition" | "switch" => self.execute_condition(node, context),
            "template" | "transform" => self.execute_template(node, context),
            "pii_filter" => self.execute_pii_filter(node, context).await,
            "tts_clean" | "text_clean" => self.execute_tts_clean(node, context).await,
            other => {
                // Deleguj do adaptera z rejestru
                if let Some(adapter) = self.registry.get(other) {
                    let config = node.config.clone();
                    let result = adapter.execute_dyn(&config, context).await?;
                    Ok(result)
                } else {
                    warn!(node_type = other, node_id = %node.id, "Brak adaptera - pass-through");
                    Ok(serde_json::Value::Null)
                }
            }
        }
    }

    /// Wezel trigger - punkt wejscia flow
    fn execute_trigger(&self, node: &FlowNode, context: &FlowContext) -> Result<serde_json::Value> {
        debug!(node_id = %node.id, "Trigger: rozpoczecie flow");
        Ok(super::adapters::trigger::build_trigger_output(context))
    }

    /// Wezel przekazujacy dane (output, router) - roznia sie tylko typem wyniku
    fn execute_passthrough(
        &self,
        node: &FlowNode,
        context: &FlowContext,
        type_name: &str,
    ) -> Result<serde_json::Value> {
        debug!(node_id = %node.id, type_name = type_name, "Passthrough: przekazanie danych");
        Ok(super::adapters::output::build_passthrough_output(
            node, context, type_name,
        ))
    }

    /// Condition/Switch - ewaluuje warunek i zwraca wynik
    fn execute_condition(
        &self,
        node: &FlowNode,
        context: &FlowContext,
    ) -> Result<serde_json::Value> {
        debug!(node_id = %node.id, "Condition: ewaluacja warunku");
        Ok(super::adapters::condition::build_condition_output(
            node, context,
        ))
    }

    /// Template - formatowanie tekstu z podstawianiem zmiennych
    fn execute_template(
        &self,
        node: &FlowNode,
        context: &FlowContext,
    ) -> Result<serde_json::Value> {
        let template_str = node
            .config
            .get("template")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut result = template_str.to_string();

        for (key, value) in &context.variables {
            let placeholder = format!("{{{}}}", key);
            let replacement = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }

        result = result.replace("{input}", &context.input);
        result = result.replace("{model}", &context.model);
        result = result.replace("{request_id}", &context.request_id);

        debug!(node_id = %node.id, "Template: sformatowano tekst");

        Ok(serde_json::json!({
            "type": "template_output",
            "text": result,
        }))
    }

    /// PII Filter - deleguje do adapters::pii_filter::apply_pii_filter.
    async fn execute_pii_filter(
        &self,
        node: &FlowNode,
        context: &mut FlowContext,
    ) -> Result<serde_json::Value> {
        super::adapters::pii_filter::apply_pii_filter(&self.db, node, context).await
    }

    /// TTS Clean - deleguje do adapters::tts_clean::apply_tts_clean.
    async fn execute_tts_clean(
        &self,
        node: &FlowNode,
        context: &FlowContext,
    ) -> Result<serde_json::Value> {
        super::adapters::tts_clean::apply_tts_clean(&self.db, node, context).await
    }

    /// Ewaluuje warunki na krawedziach wychodzacych z wezla condition/switch.
    /// blocked_edges uzywa zakodowanych indeksow (from_pos * n + to_pos) zamiast
    /// alokacji String przy kazdym wstawieniu — iteruje po `outgoing_edges_per_pos`
    /// zamiast lookup po node_id.
    fn evaluate_condition_edges(
        from_pos: usize,
        condition_result: &serde_json::Value,
        parsed: &ParsedFlow,
        edges: &[FlowEdge],
        n: usize,
        blocked_edges: &mut HashSet<usize>,
    ) {
        let result_bool = condition_result
            .get("result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let result_str: String = condition_result
            .get("result")
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default();

        for &edge_idx in &parsed.outgoing_edges_per_pos[from_pos] {
            let edge = &edges[edge_idx];
            let should_follow = match &edge.condition {
                Some(cond) => {
                    let cond_lower = cond.to_lowercase();
                    if cond_lower == "true" {
                        result_bool
                    } else if cond_lower == "false" {
                        !result_bool
                    } else {
                        result_str == *cond
                    }
                }
                None => true,
            };

            if !should_follow {
                let to_pos = parsed
                    .node_pos_by_id
                    .get(edge.to.as_str())
                    .copied()
                    .unwrap_or(0);
                blocked_edges.insert(from_pos * n + to_pos);
            }
        }
    }

}

/// Czy dany typ node'a jest pass-through na stream path — executor w S4b
/// zwraca stream bezposrednio do klienta bez transformacji per-node, wiec
/// tylko sink-typy sa dozwolone za producentem streamu.
fn is_stream_passthrough(node_type: &str) -> bool {
    matches!(
        node_type,
        "output" | "end" | "http_response" | "quic_response" | "stream_chunk"
    )
}

/// Bezpiecznie obcina string do zadanej liczby znakow (nie bajtow).
/// Unika panicu na wielobajtowych znakach UTF-8.
fn truncate_utf8(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => format!("{}...", &s[..byte_idx]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::adapters::condition::{compare_numbers, evaluate_condition};
    use crate::flow_engine::adapters::{AdapterRegistry, NodeAdapter};
    use crate::flow_engine::types::{FlowContext, FlowDefinition, FlowEdge, FlowNode};
    use std::sync::Arc;

    fn create_test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE flows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                description TEXT,
                version INTEGER DEFAULT 1,
                is_default INTEGER NOT NULL DEFAULT 0,
                service_type TEXT,
                flow_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'draft',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE flow_executions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                flow_id INTEGER NOT NULL,
                request_id TEXT,
                model TEXT,
                started_at TEXT,
                finished_at TEXT,
                status TEXT,
                execution_log TEXT,
                total_latency_ms INTEGER,
                total_tokens INTEGER
            );
            CREATE TABLE pii_rules (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                category TEXT NOT NULL,
                pattern TEXT NOT NULL,
                replacement TEXT NOT NULL DEFAULT '[UKRYTY]',
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0,
                description TEXT,
                test_examples TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE tts_cleaning_rules (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_type TEXT NOT NULL,
                pattern TEXT NOT NULL,
                replacement TEXT,
                language TEXT NOT NULL DEFAULT 'pl',
                is_active INTEGER NOT NULL DEFAULT 1,
                priority INTEGER DEFAULT 0
            );
            ",
        )
        .unwrap();
        Arc::new(std::sync::Mutex::new(conn))
    }

    fn create_test_flow(flow_json: &str) -> crate::db::models::DbFlow {
        crate::db::models::DbFlow {
            id: 1,
            name: "test-flow".to_string(),
            description: None,
            version: 1,
            is_default: false,
            service_type: Some("chat".to_string()),
            flow_json: flow_json.to_string(),
            status: "active".to_string(),
            created_at: "2025-01-01".to_string(),
            updated_at: "2025-01-01".to_string(),
        }
    }

    fn make_node(id: &str, node_type: &str, config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: id.to_string(),
            node_type: node_type.to_string(),
            config,
            position: None,
            label: None,
        }
    }

    fn make_edge(from: &str, to: &str) -> FlowEdge {
        FlowEdge {
            id: None,
            from: from.to_string(),
            to: to.to_string(),
            label: None,
            condition: None,
            from_port: "full".to_string(),
            to_port: "in".to_string(),
        }
    }

    struct MockAdapter {
        node_type_name: &'static str,
        response: serde_json::Value,
    }

    impl NodeAdapter for MockAdapter {
        fn execute(
            &self,
            _node_config: &serde_json::Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
            let resp = self.response.clone();
            async move { Ok(resp) }
        }

        fn node_type(&self) -> &'static str {
            self.node_type_name
        }
    }

    struct FailingAdapter {
        node_type_name: &'static str,
    }

    impl NodeAdapter for FailingAdapter {
        fn execute(
            &self,
            _node_config: &serde_json::Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
            async { anyhow::bail!("Symulowany blad adaptera") }
        }

        fn node_type(&self) -> &'static str {
            self.node_type_name
        }
    }

    #[test]
    fn parse_flow_prawidlowy_json_zwraca_definicje() {
        let json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "llm1", "type": "llm", "config": {"service_name": "bielik"}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "llm1"},
                {"from": "llm1", "to": "out"}
            ]
        }"#;

        let result = FlowExecutorAsync::parse_flow(json);

        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.nodes.len(), 3);
        assert_eq!(def.edges.len(), 2);
        assert_eq!(def.nodes[0].id, "t1");
        assert_eq!(def.nodes[0].node_type, "trigger");
    }

    #[test]
    fn parse_flow_nieprawidlowy_json_zwraca_blad() {
        let json = r#"{ invalid json }"#;
        let result = FlowExecutorAsync::parse_flow(json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_flow_puste_wezly_zwraca_blad() {
        let json = r#"{"nodes": [], "edges": []}"#;
        let result = FlowExecutorAsync::parse_flow(json);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("nie zawiera zadnych wezlow"));
    }

    #[test]
    fn parse_flow_krawedz_do_nieistniejacego_wezla_zwraca_blad() {
        let json = r#"{
            "nodes": [{"id": "a", "type": "trigger", "config": {}}],
            "edges": [{"from": "a", "to": "nieistniejacy"}]
        }"#;
        let result = FlowExecutorAsync::parse_flow(json);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("nieistniejacy wezel docelowy"));
    }

    #[test]
    fn parse_flow_krawedz_z_nieistniejacego_wezla_zwraca_blad() {
        let json = r#"{
            "nodes": [{"id": "a", "type": "trigger", "config": {}}],
            "edges": [{"from": "nieistniejacy", "to": "a"}]
        }"#;
        let result = FlowExecutorAsync::parse_flow(json);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("nieistniejacy wezel zrodlowy"));
    }

    #[test]
    fn topological_sort_liniowy_abc_zachowuje_kolejnosc() {
        let def = FlowDefinition {
            nodes: vec![
                make_node("a", "trigger", serde_json::Value::Null),
                make_node("b", "llm", serde_json::Value::Null),
                make_node("c", "output", serde_json::Value::Null),
            ],
            edges: vec![make_edge("a", "b"), make_edge("b", "c")],
        };

        let order = FlowExecutorAsync::topological_sort(&def).unwrap();

        assert_eq!(order.len(), 3);
        let pos_a = order.iter().position(|x| x == "a").unwrap();
        let pos_b = order.iter().position(|x| x == "b").unwrap();
        let pos_c = order.iter().position(|x| x == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn topological_sort_cykl_zwraca_blad() {
        let def = FlowDefinition {
            nodes: vec![
                make_node("a", "trigger", serde_json::Value::Null),
                make_node("b", "llm", serde_json::Value::Null),
            ],
            edges: vec![make_edge("a", "b"), make_edge("b", "a")],
        };

        let result = FlowExecutorAsync::topological_sort(&def);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("cykl"));
    }

    #[test]
    fn topological_sort_diamond_zachowuje_zaleznosci() {
        let def = FlowDefinition {
            nodes: vec![
                make_node("a", "trigger", serde_json::Value::Null),
                make_node("b", "llm", serde_json::Value::Null),
                make_node("c", "pii_filter", serde_json::Value::Null),
                make_node("d", "output", serde_json::Value::Null),
            ],
            edges: vec![
                make_edge("a", "b"),
                make_edge("a", "c"),
                make_edge("b", "d"),
                make_edge("c", "d"),
            ],
        };

        let order = FlowExecutorAsync::topological_sort(&def).unwrap();
        assert_eq!(order.len(), 4);
        let pos_a = order.iter().position(|x| x == "a").unwrap();
        let pos_b = order.iter().position(|x| x == "b").unwrap();
        let pos_c = order.iter().position(|x| x == "c").unwrap();
        let pos_d = order.iter().position(|x| x == "d").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_a < pos_c);
        assert!(pos_b < pos_d);
        assert!(pos_c < pos_d);
    }

    #[test]
    fn topological_sort_pojedynczy_wezel_zwraca_go() {
        let def = FlowDefinition {
            nodes: vec![make_node("only", "trigger", serde_json::Value::Null)],
            edges: vec![],
        };
        let order = FlowExecutorAsync::topological_sort(&def).unwrap();
        assert_eq!(order, vec!["only"]);
    }

    #[test]
    fn topological_sort_trojcyklowy_zwraca_blad() {
        let def = FlowDefinition {
            nodes: vec![
                make_node("a", "trigger", serde_json::Value::Null),
                make_node("b", "llm", serde_json::Value::Null),
                make_node("c", "output", serde_json::Value::Null),
            ],
            edges: vec![
                make_edge("a", "b"),
                make_edge("b", "c"),
                make_edge("c", "a"),
            ],
        };
        let result = FlowExecutorAsync::topological_sort(&def);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_prosty_flow_trigger_llm_output_zwraca_wynik() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(MockAdapter {
            node_type_name: "llm",
            response: serde_json::json!({
                "text": "Odpowiedz z mocka LLM",
                "tokens": {"prompt": 10, "completion": 20}
            }),
        });

        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "llm1", "type": "llm", "config": {"service_name": "bielik"}},
                {"id": "out", "type": "output", "config": {"input_from": "llm1"}}
            ],
            "edges": [
                {"from": "t1", "to": "llm1"},
                {"from": "llm1", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-001".to_string(),
            "bielik-11b".to_string(),
            "Cześć, jak sie masz?".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.status, "completed");
        assert_eq!(exec_result.total_tokens, 30);
        assert_eq!(exec_result.execution_log.len(), 3);
        let output_text = exec_result.output.get("text").and_then(|v| v.as_str());
        assert_eq!(output_text, Some("Odpowiedz z mocka LLM"));
    }

    #[tokio::test]
    async fn execute_flow_z_brakujacym_adapterem_kontynuuje() {
        let db = create_test_db();
        let registry = AdapterRegistry::new();
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "svc", "type": "custom_svc", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "svc"},
                {"from": "svc", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-002".to_string(),
            "bielik-11b".to_string(),
            "Testowy input".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.status, "completed");
        assert_eq!(exec_result.execution_log.len(), 3);
    }

    #[tokio::test]
    async fn execute_flow_z_condition_true_branch_prawidlowe_branchowanie() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(MockAdapter {
            node_type_name: "rag",
            response: serde_json::json!({"text": "Wynik RAG", "tokens": {"prompt": 5, "completion": 10}}),
        });
        registry.register(MockAdapter {
            node_type_name: "llm",
            response: serde_json::json!({"text": "Wynik LLM", "tokens": {"prompt": 5, "completion": 10}}),
        });

        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "cond", "type": "condition", "config": {"field": "input", "operator": "contains", "value": "rag"}},
                {"id": "rag_node", "type": "rag", "config": {}},
                {"id": "llm_node", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {"input_from": "rag_node"}}
            ],
            "edges": [
                {"from": "t1", "to": "cond"},
                {"from": "cond", "to": "rag_node", "condition": "true"},
                {"from": "cond", "to": "llm_node", "condition": "false"},
                {"from": "rag_node", "to": "out"},
                {"from": "llm_node", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-003".to_string(),
            "bielik-11b".to_string(),
            "Uzyj rag do odpowiedzi".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.status, "completed");
        let skipped = exec_result
            .execution_log
            .iter()
            .find(|s| s.node_id == "llm_node");
        assert!(skipped.is_none(), "llm_node powinien byc pominiety");
        let rag_step = exec_result
            .execution_log
            .iter()
            .find(|s| s.node_id == "rag_node");
        assert!(rag_step.is_some(), "rag_node powinien sie wykonac");
    }

    #[tokio::test]
    async fn execute_flow_z_condition_false_branch() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(MockAdapter {
            node_type_name: "rag",
            response: serde_json::json!({"text": "Wynik RAG"}),
        });
        registry.register(MockAdapter {
            node_type_name: "llm",
            response: serde_json::json!({"text": "Wynik LLM"}),
        });

        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "cond", "type": "condition", "config": {"field": "input", "operator": "contains", "value": "rag"}},
                {"id": "rag_node", "type": "rag", "config": {}},
                {"id": "llm_node", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "cond"},
                {"from": "cond", "to": "rag_node", "condition": "true"},
                {"from": "cond", "to": "llm_node", "condition": "false"},
                {"from": "rag_node", "to": "out"},
                {"from": "llm_node", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-004".to_string(),
            "bielik-11b".to_string(),
            "Zwykly chat bez specjalnych slow".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        let rag_step = exec_result
            .execution_log
            .iter()
            .find(|s| s.node_id == "rag_node");
        assert!(rag_step.is_none(), "rag_node powinien byc pominiety");
        let llm_step = exec_result
            .execution_log
            .iter()
            .find(|s| s.node_id == "llm_node");
        assert!(llm_step.is_some(), "llm_node powinien sie wykonac");
    }

    #[tokio::test]
    async fn execute_flow_z_template_prawidlowa_substytucja() {
        let db = create_test_db();
        let registry = AdapterRegistry::new();
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "tpl", "type": "template", "config": {"template": "Model: {model}, Input: {input}, Jezyk: {lang}"}},
                {"id": "out", "type": "output", "config": {"input_from": "tpl"}}
            ],
            "edges": [
                {"from": "t1", "to": "tpl"},
                {"from": "tpl", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-005".to_string(),
            "bielik-11b".to_string(),
            "Pytanie testowe".to_string(),
        );
        ctx.variables.insert(
            "lang".to_string(),
            serde_json::Value::String("pl".to_string()),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.status, "completed");
        let tpl_result = ctx.node_results.get("tpl").unwrap();
        let tpl_text = tpl_result.get("text").and_then(|v| v.as_str()).unwrap();
        assert!(tpl_text.contains("bielik-11b"));
        assert!(tpl_text.contains("Pytanie testowe"));
        assert!(tpl_text.contains("pl"));
    }

    #[tokio::test]
    async fn execute_flow_z_blednym_adapterem_status_error() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(FailingAdapter {
            node_type_name: "llm",
        });

        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "llm1"},
                {"from": "llm1", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-006".to_string(),
            "bielik-11b".to_string(),
            "test".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.status, "error");
        let error_step = exec_result
            .execution_log
            .iter()
            .find(|s| s.status == "error");
        assert!(error_step.is_some());
    }

    #[tokio::test]
    async fn execute_flow_zlicza_tokeny_z_wielu_adapterow() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(MockAdapter {
            node_type_name: "rag",
            response: serde_json::json!({
                "text": "Kontekst RAG",
                "tokens": {"prompt": 15, "completion": 25}
            }),
        });
        registry.register(MockAdapter {
            node_type_name: "llm",
            response: serde_json::json!({
                "text": "Odpowiedz LLM",
                "tokens": {"prompt": 30, "completion": 50}
            }),
        });

        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "rag1", "type": "rag", "config": {}},
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "rag1"},
                {"from": "rag1", "to": "llm1"},
                {"from": "llm1", "to": "out"}
            ]
        }"#;

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (1, 'test', ?1, 'active')",
                rusqlite::params![flow_json],
            )
            .unwrap();
        }

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-007".to_string(),
            "bielik-11b".to_string(),
            "Pytanie".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let result = executor.execute(&flow, &parsed, &mut ctx).await;

        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.total_tokens, 120);
    }

    #[test]
    fn evaluate_condition_equals_zgodne_wartosci_zwraca_true() {
        let a = serde_json::json!("hello");
        let b = serde_json::json!("hello");
        assert!(evaluate_condition(&a, "equals", &b));
        assert!(evaluate_condition(&a, "eq", &b));
        assert!(evaluate_condition(&a, "==", &b));
    }

    #[test]
    fn evaluate_condition_not_equals_rozne_wartosci_zwraca_true() {
        let a = serde_json::json!("hello");
        let b = serde_json::json!("world");
        assert!(evaluate_condition(&a, "not_equals", &b));
        assert!(evaluate_condition(&a, "!=", &b));
    }

    #[test]
    fn evaluate_condition_contains_podciag_zwraca_true() {
        let a = serde_json::json!("Cześć, jak się masz?");
        let b = serde_json::json!("jak się");
        assert!(evaluate_condition(&a, "contains", &b));
    }

    #[test]
    fn evaluate_condition_contains_brak_podciagu_zwraca_false() {
        let a = serde_json::json!("Cześć");
        let b = serde_json::json!("brak");
        assert!(!evaluate_condition(&a, "contains", &b));
    }

    #[test]
    fn evaluate_condition_gt_wieksza_liczba_zwraca_true() {
        assert!(evaluate_condition(
            &serde_json::json!(10),
            "gt",
            &serde_json::json!(5)
        ));
        assert!(!evaluate_condition(
            &serde_json::json!(3),
            "gt",
            &serde_json::json!(5)
        ));
    }

    #[test]
    fn evaluate_condition_exists_wartosc_nienull_zwraca_true() {
        assert!(evaluate_condition(
            &serde_json::json!("abc"),
            "exists",
            &serde_json::Value::Null
        ));
        assert!(!evaluate_condition(
            &serde_json::Value::Null,
            "exists",
            &serde_json::Value::Null
        ));
    }

    #[test]
    fn evaluate_condition_is_empty_pusty_string_zwraca_true() {
        assert!(evaluate_condition(
            &serde_json::json!(""),
            "is_empty",
            &serde_json::Value::Null
        ));
        assert!(evaluate_condition(
            &serde_json::Value::Null,
            "is_empty",
            &serde_json::Value::Null
        ));
        assert!(!evaluate_condition(
            &serde_json::json!("abc"),
            "is_empty",
            &serde_json::Value::Null
        ));
    }

    #[test]
    fn evaluate_condition_is_empty_pusta_tablica_zwraca_true() {
        assert!(evaluate_condition(
            &serde_json::json!([]),
            "is_empty",
            &serde_json::Value::Null
        ));
        assert!(!evaluate_condition(
            &serde_json::json!([1, 2]),
            "is_empty",
            &serde_json::Value::Null
        ));
    }

    #[test]
    fn evaluate_condition_nieznany_operator_zwraca_false() {
        assert!(!evaluate_condition(
            &serde_json::json!("a"),
            "unknown_op",
            &serde_json::json!("a")
        ));
    }

    #[test]
    fn compare_numbers_poprawne_porownanie_float() {
        assert!(compare_numbers(
            &serde_json::json!(3.14),
            &serde_json::json!(2.71),
            |a, b| a > b
        ));
    }

    #[test]
    fn compare_numbers_nienumeryczne_wartosci_zwraca_false() {
        assert!(!compare_numbers(
            &serde_json::json!("abc"),
            &serde_json::json!(5),
            |a, b| a > b
        ));
    }

    // === Streaming flow tests (S4b) ===

    use crate::api::openai::types::{ChatCompletionChunk, ChunkChoice, Delta};
    use crate::flow_engine::adapters::AdapterChunkStream;
    use futures::stream::StreamExt;

    /// Adapter ktory produkuje zdefiniowana liste chunkow na wywolanie streaming.
    struct StreamingMockAdapter {
        node_type_name: &'static str,
        chunks: Vec<String>,
    }

    impl NodeAdapter for StreamingMockAdapter {
        fn execute(
            &self,
            _c: &serde_json::Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send {
            async { Ok(serde_json::json!({"text": "blocking-fallback"})) }
        }

        fn node_type(&self) -> &'static str {
            self.node_type_name
        }

        fn supported_output_ports(&self) -> &'static [&'static str] {
            &["stream", "full"]
        }

        fn execute_streaming(
            &self,
            _node_config: &serde_json::Value,
            _ctx: &mut FlowContext,
        ) -> impl std::future::Future<Output = Option<Result<AdapterChunkStream>>> + Send {
            let chunks = self.chunks.clone();
            async move {
                let items: Vec<Result<ChatCompletionChunk>> = chunks
                    .into_iter()
                    .map(|text| {
                        Ok(ChatCompletionChunk {
                            id: "test".to_string(),
                            object: "chat.completion.chunk".to_string(),
                            created: 0,
                            model: "m".to_string(),
                            choices: vec![ChunkChoice {
                                index: 0,
                                delta: Delta {
                                    role: None,
                                    content: Some(text),
                                    reasoning_content: None,
                                    tool_calls: None,
                                },
                                finish_reason: None,
                                logprobs: None,
                            }],
                            system_fingerprint: None,
                            audio: None,
                            detected_intent: None,
                            detected_tools: None,
                            transcribed_text: None,
                            speaker_id: None,
                            speaker_name: None,
                        })
                    })
                    .collect();
                let s = futures::stream::iter(items);
                let boxed: AdapterChunkStream = Box::pin(s);
                Some(Ok(boxed))
            }
        }
    }

    #[tokio::test]
    async fn execute_streaming_flow_forwarduje_chunki() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(StreamingMockAdapter {
            node_type_name: "llm",
            chunks: vec!["Hel".to_string(), "lo".to_string(), "!".to_string()],
        });
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "llm1"},
                {"from": "llm1", "to": "out", "from_port": "stream", "to_port": "in"}
            ]
        }"#;

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new(
            "req-stream-1".to_string(),
            "any".to_string(),
            "Hej".to_string(),
        );

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let stream = executor
            .execute_streaming_flow(&flow, &parsed, &mut ctx)
            .await
            .expect("streaming flow should start");

        let collected: Vec<_> = stream.collect().await;
        assert_eq!(collected.len(), 3);
        assert_eq!(
            collected[0].as_ref().unwrap().choices[0].delta.content.as_deref(),
            Some("Hel")
        );
        assert_eq!(
            collected[2].as_ref().unwrap().choices[0].delta.content.as_deref(),
            Some("!")
        );
    }

    #[tokio::test]
    async fn execute_streaming_flow_odrzuca_flow_bez_stream_edge() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(StreamingMockAdapter {
            node_type_name: "llm",
            chunks: vec![],
        });
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "t1", "type": "trigger", "config": {}},
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t1", "to": "llm1"},
                {"from": "llm1", "to": "out"}
            ]
        }"#;

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new("r".to_string(), "m".to_string(), "i".to_string());

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let res = executor.execute_streaming_flow(&flow, &parsed, &mut ctx).await;
        let msg = match res {
            Err(e) => e.to_string(),
            Ok(_) => panic!("flow bez stream edge powinien dac blad"),
        };
        assert!(msg.contains("stream"), "error zawiera 'stream': {}", msg);
    }

    #[tokio::test]
    async fn execute_streaming_flow_odrzuca_transforming_node_po_producencie() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        registry.register(StreamingMockAdapter {
            node_type_name: "llm",
            chunks: vec![],
        });
        // 'rag' to node nie-passthrough — nie moze lezec za producentem na stream
        registry.register(MockAdapter {
            node_type_name: "rag",
            response: serde_json::json!({"text": "x"}),
        });
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "rag1", "type": "rag", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "llm1", "to": "rag1", "from_port": "stream", "to_port": "in"},
                {"from": "rag1", "to": "out"}
            ]
        }"#;

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new("r".to_string(), "m".to_string(), "i".to_string());

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let res = executor.execute_streaming_flow(&flow, &parsed, &mut ctx).await;
        let msg = match res {
            Err(e) => e.to_string(),
            Ok(_) => panic!("powinno zwrocic blad"),
        };
        assert!(
            msg.contains("pass-through") || msg.contains("passthrough"),
            "error dot. pass-through: {}",
            msg
        );
    }

    #[tokio::test]
    async fn execute_streaming_flow_odrzuca_adapter_bez_streamingu() {
        let db = create_test_db();
        let mut registry = AdapterRegistry::new();
        // Mock adapter NIE implementuje execute_streaming — default None
        registry.register(MockAdapter {
            node_type_name: "llm",
            response: serde_json::json!({"text": "x"}),
        });
        let executor = FlowExecutorAsync::new(db.clone(), Arc::new(registry));

        let flow_json = r#"{
            "nodes": [
                {"id": "llm1", "type": "llm", "config": {}},
                {"id": "out", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "llm1", "to": "out", "from_port": "stream", "to_port": "in"}
            ]
        }"#;

        let flow = create_test_flow(flow_json);
        let mut ctx = FlowContext::new("r".to_string(), "m".to_string(), "i".to_string());

        let parsed = ParsedFlow::parse(&flow.flow_json).unwrap();
        let res = executor.execute_streaming_flow(&flow, &parsed, &mut ctx).await;
        let msg = match res {
            Err(e) => e.to_string(),
            Ok(_) => panic!("powinno zwrocic blad"),
        };
        assert!(
            msg.contains("nie wspiera streamingu"),
            "error dot. braku streamingu: {}",
            msg
        );
    }
}

// =============================================================================
// Plik: flow_engine/executor_async.rs
// Opis: Asynchroniczny executor flow DAG - parsuje definicje, sortuje
//       topologicznie i wykonuje wezly przez AdapterRegistry. Zastepuje
//       synchroniczny FlowEngine dla wezlow serwisowych (LLM, RAG, STT itd.).
// =============================================================================

use super::adapters::AdapterRegistry;
use super::types::{
    FlowContext, FlowDefinition, FlowEdge, FlowExecutionResult, FlowNode, FlowStepLog,
};
use crate::db::repository;
use crate::db::DbPool;
use anyhow::{bail, Result};
use chrono::Utc;
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, info, warn};

const MAX_FLOW_NODES: usize = 256;
const MAX_FLOW_EDGES: usize = 1024;
const REGEX_SIZE_LIMIT: usize = 1_000_000;

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
    fn parse_flow(flow_json: &str) -> Result<FlowDefinition> {
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
    fn topological_sort(definition: &FlowDefinition) -> Result<Vec<String>> {
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

    /// Wykonuje flow od poczatku do konca (async).
    /// Tworzy rekord execution w DB, przetwarza wezly wg porzadku topologicznego
    /// delegujac serwisowe wezly do adapterow, aktualizuje rekord po zakonczeniu.
    pub async fn execute(
        &self,
        flow: &crate::db::models::DbFlow,
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

        let definition = Self::parse_flow(&flow.flow_json)?;
        let execution_order = Self::topological_sort(&definition)?;

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

        // Pre-computed mapa krawedzi wchodzacych (unika alokacji Vec w kazdej iteracji)
        let incoming_edges: HashMap<&str, Vec<&FlowEdge>> = {
            let mut map: HashMap<&str, Vec<&FlowEdge>> = HashMap::new();
            for edge in &definition.edges {
                map.entry(edge.to.as_str()).or_default().push(edge);
            }
            map
        };

        // Uzywamy indeksow do execution_order zamiast klonowania Stringow
        let mut blocked_edges: HashSet<usize> = HashSet::new();
        // Mapa edge index: (from_idx, to_idx) w execution_order
        let node_idx_map: HashMap<&str, usize> = execution_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let mut executed_nodes: HashSet<usize> = HashSet::new();
        let mut final_output = serde_json::Value::Null;
        let mut total_prompt_tokens: i64 = 0;
        let mut total_completion_tokens: i64 = 0;

        // Pre-compute indeksy krawedzi do blokowania (edge_index = from_idx * N + to_idx)
        let n = execution_order.len();

        for (current_idx, node_id) in execution_order.iter().enumerate() {
            // Lazy evaluation: wezel wykonuje sie jesli CO NAJMNIEJ JEDNA krawedz wejsciowa jest aktywna
            if let Some(incoming) = incoming_edges.get(node_id.as_str()) {
                let has_active_input = incoming.iter().any(|e| {
                    let from_idx = node_idx_map.get(e.from.as_str()).copied().unwrap_or(0);
                    let to_idx = current_idx;
                    !blocked_edges.contains(&(from_idx * n + to_idx))
                        && executed_nodes.contains(&from_idx)
                });
                if !has_active_input {
                    debug!(node_id = %node_id, "Pomijam wezel (wszystkie wejscia zablokowane)");
                    continue;
                }
            }

            let node = match node_map.get(node_id.as_str()) {
                Some(n) => *n,
                None => {
                    warn!(node_id = %node_id, "Wezel nie znaleziony w mapie");
                    continue;
                }
            };

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

                    context.node_results.insert(node_id.clone(), output);

                    executed_nodes.insert(current_idx);

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
                            node_id,
                            cond_result,
                            &outgoing_edges,
                            &node_idx_map,
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
                        node_id: node_id.clone(),
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
                node_id: node_id.clone(),
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
        Ok(serde_json::json!({
            "input": context.input,
            "model": context.model,
            "request_id": context.request_id,
        }))
    }

    /// Wezel przekazujacy dane (output, router) - roznia sie tylko typem wyniku
    fn execute_passthrough(
        &self,
        node: &FlowNode,
        context: &FlowContext,
        type_name: &str,
    ) -> Result<serde_json::Value> {
        let text = self.resolve_input_text(node, context);
        debug!(node_id = %node.id, type_name = type_name, "Passthrough: przekazanie danych");
        Ok(serde_json::json!({
            "type": type_name,
            "text": text,
        }))
    }

    /// Condition/Switch - ewaluuje warunek i zwraca wynik
    fn execute_condition(
        &self,
        node: &FlowNode,
        context: &FlowContext,
    ) -> Result<serde_json::Value> {
        let field = node
            .config
            .get("field")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let operator = node
            .config
            .get("operator")
            .and_then(|v| v.as_str())
            .unwrap_or("equals");
        let expected = node
            .config
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let actual = self.resolve_field_value(field, context);
        let result = evaluate_condition(&actual, operator, &expected);

        debug!(
            node_id = %node.id,
            field = field,
            operator = operator,
            result = result,
            "Condition: ewaluacja warunku"
        );

        Ok(serde_json::json!({
            "type": "condition_result",
            "field": field,
            "operator": operator,
            "result": result,
        }))
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

    /// PII Filter - pobiera aktywne reguly z DB i aplikuje regex na tekscie
    async fn execute_pii_filter(
        &self,
        node: &FlowNode,
        context: &mut FlowContext,
    ) -> Result<serde_json::Value> {
        let db_clone = self.db.clone();
        let rules =
            tokio::task::spawn_blocking(move || repository::list_pii_rules_active(&db_clone))
                .await??;
        let mut text = self.resolve_input_text(node, context);

        for rule in &rules {
            match RegexBuilder::new(&rule.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
            {
                Ok(re) => {
                    let replaced = re.replace_all(&text, rule.replacement.as_str());
                    // Cow::Borrowed = brak dopasowania, Cow::Owned = tekst zmieniony
                    if let std::borrow::Cow::Owned(new_text) = replaced {
                        text = new_text;
                        debug!(
                            rule_name = %rule.name,
                            category = %rule.category,
                            "PII filter: zastosowano regule"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        rule_id = rule.id,
                        rule_name = %rule.name,
                        pattern = %rule.pattern,
                        error = %e,
                        "PII filter: niepoprawny regex w regule"
                    );
                }
            }
        }

        // Przefiltruj ostatnia wiadomosc user w ctx.messages (przed move do context.input)
        if !context.messages.is_empty() {
            if let Some(last_user_idx) = context
                .messages
                .iter()
                .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            {
                context.messages[last_user_idx] = serde_json::json!({
                    "role": "user",
                    "content": &text,
                });
            }
        }

        // Zaktualizuj kontekst na przefiltrowany tekst
        context.input = text.clone();

        debug!(
            node_id = %node.id,
            rules_count = rules.len(),
            "PII filter: przefiltrowano tekst"
        );

        Ok(serde_json::json!({
            "type": "pii_filtered",
            "text": text,
            "rules_applied": rules.len(),
        }))
    }

    /// TTS Clean - pobiera aktywne reguly czyszczenia z DB i aplikuje na tekscie
    async fn execute_tts_clean(
        &self,
        node: &FlowNode,
        context: &FlowContext,
    ) -> Result<serde_json::Value> {
        let db_clone = self.db.clone();
        let rules = tokio::task::spawn_blocking(move || {
            repository::list_tts_cleaning_rules_active(&db_clone)
        })
        .await??;
        let mut text = self.resolve_input_text(node, context);

        for rule in &rules {
            match rule.rule_type.as_str() {
                "abbreviation" => {
                    if let Some(ref replacement) = rule.replacement {
                        text = text.replace(&rule.pattern, replacement);
                    }
                }
                "regex_remove" | "emoji_range" => {
                    match RegexBuilder::new(&rule.pattern)
                        .size_limit(REGEX_SIZE_LIMIT)
                        .build()
                    {
                        Ok(re) => {
                            let replacement = rule.replacement.as_deref().unwrap_or("");
                            text = re.replace_all(&text, replacement).to_string();
                        }
                        Err(e) => {
                            warn!(
                                rule_id = rule.id,
                                pattern = %rule.pattern,
                                error = %e,
                                "TTS clean: niepoprawny regex"
                            );
                        }
                    }
                }
                "phonetic" => {
                    if let Some(ref replacement) = rule.replacement {
                        text = text.replace(&rule.pattern, replacement);
                    }
                }
                other => {
                    debug!(rule_type = other, "TTS clean: nieznany typ reguly");
                }
            }
        }

        debug!(
            node_id = %node.id,
            rules_count = rules.len(),
            "TTS clean: wyczyszczono tekst"
        );

        Ok(serde_json::json!({
            "type": "tts_cleaned",
            "text": text,
            "rules_applied": rules.len(),
        }))
    }

    /// Ewaluuje warunki na krawedziach wychodzacych z wezla condition/switch.
    /// blocked_edges uzywa zakodowanych indeksow (from_idx * n + to_idx) zamiast
    /// alokacji String przy kazdym wstawieniu.
    fn evaluate_condition_edges(
        node_id: &str,
        condition_result: &serde_json::Value,
        outgoing_edges: &HashMap<&str, Vec<&FlowEdge>>,
        node_idx_map: &HashMap<&str, usize>,
        n: usize,
        blocked_edges: &mut HashSet<usize>,
    ) {
        let result_bool = condition_result
            .get("result")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if let Some(edges) = outgoing_edges.get(node_id) {
            let from_idx = node_idx_map.get(node_id).copied().unwrap_or(0);

            // Oblicz raz przed petla - unika alokacji String w kazdej iteracji
            let result_str: String = condition_result
                .get("result")
                .map(|v| v.to_string().trim_matches('"').to_string())
                .unwrap_or_default();

            for edge in edges {
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
                    let to_idx = node_idx_map.get(edge.to.as_str()).copied().unwrap_or(0);
                    blocked_edges.insert(from_idx * n + to_idx);
                }
            }
        }
    }

    /// Pobiera tekst wejsciowy dla wezla - szuka wstecz w execution_log
    /// az znajdzie node z polem "text" (pomija condition, side-effect nody)
    fn resolve_input_text(&self, node: &FlowNode, context: &FlowContext) -> String {
        if let Some(input_from) = node.config.get("input_from").and_then(|v| v.as_str()) {
            if let Some(prev_result) = context.node_results.get(input_from) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
                return prev_result.to_string();
            }
        }

        for step in context.execution_log.iter().rev() {
            if let Some(prev_result) = context.node_results.get(&step.node_id) {
                if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                    return text.to_string();
                }
            }
        }

        context.input.clone()
    }

    /// Pobiera wartosc pola z kontekstu - wspiera auto-resolve z predecessora
    /// i zagniezdzona notacje kropkowa (np. "tokens.prompt")
    fn resolve_field_value(&self, field: &str, context: &FlowContext) -> serde_json::Value {
        if field == "input" {
            return serde_json::Value::String(context.input.clone());
        }
        if field == "model" {
            return serde_json::Value::String(context.model.clone());
        }

        if let Some(val) = context.variables.get(field) {
            return val.clone();
        }

        // Backward compat: jawne node_id.pole (np. "n6.should_query")
        if let Some((prefix, rest)) = field.split_once('.') {
            if context.node_results.contains_key(prefix) {
                return resolve_json_path(context.node_results.get(prefix).unwrap(), rest);
            }
        }

        // Auto-resolve: szukaj pola w outputach wezlow wstecz
        for step in context.execution_log.iter().rev() {
            if let Some(result) = context.node_results.get(&step.node_id) {
                let resolved = resolve_json_path(result, field);
                if !resolved.is_null() {
                    return resolved;
                }
            }
        }

        serde_json::Value::Null
    }
}

/// Rozwiazuje sciezke JSON z notacja kropkowa (np. "tokens.prompt")
fn resolve_json_path(value: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut current = value;
    for key in path.split('.') {
        match current.get(key) {
            Some(v) => current = v,
            None => return serde_json::Value::Null,
        }
    }
    current.clone()
}

/// Bezpiecznie obcina string do zadanej liczby znakow (nie bajtow).
/// Unika panicu na wielobajtowych znakach UTF-8.
fn truncate_utf8(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => format!("{}...", &s[..byte_idx]),
        None => s.to_string(),
    }
}

/// Ewaluuje prosty warunek porownania
fn evaluate_condition(
    actual: &serde_json::Value,
    operator: &str,
    expected: &serde_json::Value,
) -> bool {
    match operator {
        "equals" | "eq" | "==" => actual == expected,
        "not_equals" | "neq" | "!=" => actual != expected,
        "contains" => {
            if let (Some(haystack), Some(needle)) = (actual.as_str(), expected.as_str()) {
                haystack.contains(needle)
            } else {
                false
            }
        }
        "not_contains" => {
            if let (Some(haystack), Some(needle)) = (actual.as_str(), expected.as_str()) {
                !haystack.contains(needle)
            } else {
                true
            }
        }
        "gt" | ">" => compare_numbers(actual, expected, |a, b| a > b),
        "gte" | ">=" => compare_numbers(actual, expected, |a, b| a >= b),
        "lt" | "<" => compare_numbers(actual, expected, |a, b| a < b),
        "lte" | "<=" => compare_numbers(actual, expected, |a, b| a <= b),
        "exists" => !actual.is_null(),
        "not_exists" => actual.is_null(),
        "is_empty" => {
            actual.is_null()
                || actual.as_str().map_or(false, |s| s.is_empty())
                || actual.as_array().map_or(false, |a| a.is_empty())
        }
        "is_not_empty" => {
            !actual.is_null()
                && !actual.as_str().map_or(false, |s| s.is_empty())
                && !actual.as_array().map_or(false, |a| a.is_empty())
        }
        _ => {
            warn!(operator = operator, "Nieznany operator warunku");
            false
        }
    }
}

/// Porownuje dwie wartosci JSON jako liczby
fn compare_numbers(
    a: &serde_json::Value,
    b: &serde_json::Value,
    cmp: fn(f64, f64) -> bool,
) -> bool {
    let a_num = a.as_f64().or_else(|| a.as_i64().map(|i| i as f64));
    let b_num = b.as_f64().or_else(|| b.as_i64().map(|i| i as f64));

    match (a_num, b_num) {
        (Some(av), Some(bv)) => cmp(av, bv),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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

        let result = executor.execute(&flow, &mut ctx).await;

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
}

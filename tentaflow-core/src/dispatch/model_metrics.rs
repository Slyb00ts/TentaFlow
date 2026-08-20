// =============================================================================
// Plik: dispatch/model_metrics.rs
// Opis: Handlery binarnego API metryk modeli — zagregowane summary (histogramy
//       TTFT/decode/e2e), przekrój węzeł×serwis oraz cennik per-model. Wszystko
//       po CBOR. Agregacja mesh-wide wynika z replikowanego `model_metrics_rollup`.
// Przykład: ModelMetricsPayload::SummaryRequest zwraca zsumowane metryki grupy.
// =============================================================================

use std::collections::{HashMap, HashSet};

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ModelMetricsFilterWire, ModelMetricsPayload, ModelMetricsRowWire,
    ModelNodeServiceRowWire, ModelPricingWire, ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::db::models::{
    DbModelMetricsRollup, DbModelPricing, ModelMetricsFilter, NewModelPricing,
};
use crate::db::repository::{
    self, DECODE_TPS_EDGES, E2E_MS_EDGES, MODEL_METRICS_HISTOGRAM_VERSION, TTFT_MS_EDGES,
};
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "metrics.read";
const PERM_WRITE: &str = "metrics.write";
const NO_GROUP_KEY: &str = "(no group)";

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "metrics.read permission required",
        ));
    }
    Ok(org)
}

fn require_write(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "metrics.write permission required",
        ));
    }
    Ok(org)
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "model metrics admin database error");
    ProtocolError::internal("model metrics database error")
}

/// Percentyl `p` (np. 50/90/99) z histogramu o stałych krawędziach. `counts` to
/// ZSUMOWANE kubełki grupy, `edges` to krawędzie (ostatnia to sentinel ∞).
/// Interpolacja liniowa wewnątrz kubełka; kubełek-sentinel (ostatni) zwraca swoją
/// dolną krawędź, bo górna jest nieograniczona. Brak próbek → `None`.
fn percentile_from_histogram(
    counts: &[i64],
    edges: &[f64],
    sample_count: i64,
    p: f64,
) -> Option<f64> {
    if sample_count <= 0 {
        return None;
    }
    let target = (p / 100.0) * sample_count as f64;
    let mut cumulative_before: i64 = 0;
    for (i, &c) in counts.iter().enumerate() {
        let cumulative_after = cumulative_before + c;
        if c > 0 && (cumulative_after as f64) >= target {
            let lower = if i == 0 { 0.0 } else { edges[i - 1] };
            if i == counts.len() - 1 {
                return Some(lower);
            }
            let upper = edges[i];
            let frac = ((target - cumulative_before as f64) / c as f64).clamp(0.0, 1.0);
            return Some(lower + frac * (upper - lower));
        }
        cumulative_before = cumulative_after;
    }
    None
}

/// Krawędzie histogramów jako `f64` (dla interpolacji percentyli). i64::MAX/∞
/// pozostają sentinelem — nigdy nie są używane jako górna krawędź (kubełek
/// sentinel zwraca dolną krawędź), więc utrata precyzji jest bez znaczenia.
fn ttft_edges_f64() -> [f64; 10] {
    TTFT_MS_EDGES.map(|e| e as f64)
}
fn e2e_edges_f64() -> [f64; 10] {
    E2E_MS_EDGES.map(|e| e as f64)
}

/// Akumulator jednej grupy summary. Histogramy sumowane pokubełkowo, żeby
/// percentyle liczyć z pełnego rozkładu grupy.
#[derive(Default)]
struct SummaryAgg {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    embedding_tokens: i64,
    audio_ms: i64,
    images: i64,
    request_count: i64,
    success_count: i64,
    error_count: i64,
    usage_missing_count: i64,
    cost: f64,
    missing_pricing: bool,
    ttft: [i64; 10],
    ttft_samples: i64,
    decode: [i64; 8],
    decode_samples: i64,
    e2e: [i64; 10],
    e2e_samples: i64,
}

impl SummaryAgg {
    fn add_row(&mut self, row: &DbModelMetricsRollup, row_cost: f64, missing_pricing: bool) {
        self.missing_pricing = self.missing_pricing || missing_pricing;
        self.prompt_tokens += row.prompt_tokens;
        self.completion_tokens += row.completion_tokens;
        self.total_tokens += row.total_tokens;
        self.embedding_tokens += row.embedding_tokens;
        self.audio_ms += row.audio_ms;
        self.images += row.images;
        self.request_count += row.request_count;
        self.success_count += row.success_count;
        self.error_count += row.error_count;
        self.usage_missing_count += row.usage_missing_count;
        self.cost += row_cost;
        // Percentyle wolno liczyc tylko z jednej wersji rozkladu — sumowanie
        // kubelkow o roznych krawedziach daloby bezsensowne percentyle. Wiersze
        // starej wersji wchodza do licznikow/tokenow/kosztu, ale nie do histogramow.
        if row.histogram_version == MODEL_METRICS_HISTOGRAM_VERSION {
            for i in 0..10 {
                self.ttft[i] += row.ttft_buckets[i];
                self.e2e[i] += row.e2e_buckets[i];
            }
            for i in 0..8 {
                self.decode[i] += row.decode_tps_buckets[i];
            }
            self.ttft_samples += row.ttft_sample_count;
            self.decode_samples += row.decode_tps_sample_count;
            self.e2e_samples += row.e2e_sample_count;
        }
    }

    fn into_wire(self, key: String) -> ModelMetricsRowWire {
        let ttft_edges = ttft_edges_f64();
        let decode_edges = DECODE_TPS_EDGES;
        let e2e_edges = e2e_edges_f64();
        let error_rate = if self.request_count > 0 {
            self.error_count as f64 / self.request_count as f64
        } else {
            0.0
        };
        ModelMetricsRowWire {
            key,
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            embedding_tokens: self.embedding_tokens,
            audio_ms: self.audio_ms,
            images: self.images,
            request_count: self.request_count,
            success_count: self.success_count,
            error_count: self.error_count,
            cost: self.cost,
            missing_pricing: self.missing_pricing,
            usage_missing_count: u64::try_from(self.usage_missing_count).unwrap_or(0),
            error_rate,
            ttft_p50: percentile_from_histogram(&self.ttft, &ttft_edges, self.ttft_samples, 50.0),
            ttft_p90: percentile_from_histogram(&self.ttft, &ttft_edges, self.ttft_samples, 90.0),
            ttft_p99: percentile_from_histogram(&self.ttft, &ttft_edges, self.ttft_samples, 99.0),
            decode_p50: percentile_from_histogram(
                &self.decode,
                &decode_edges,
                self.decode_samples,
                50.0,
            ),
            decode_p90: percentile_from_histogram(
                &self.decode,
                &decode_edges,
                self.decode_samples,
                90.0,
            ),
            decode_p99: percentile_from_histogram(
                &self.decode,
                &decode_edges,
                self.decode_samples,
                99.0,
            ),
            e2e_p50: percentile_from_histogram(&self.e2e, &e2e_edges, self.e2e_samples, 50.0),
            e2e_p90: percentile_from_histogram(&self.e2e, &e2e_edges, self.e2e_samples, 90.0),
            e2e_p99: percentile_from_histogram(&self.e2e, &e2e_edges, self.e2e_samples, 99.0),
            display_name: None,
            subtitle: None,
            member_count: None,
            last_seen_at: None,
        }
    }
}

/// Resolved display data of a node: `sync_nodes.display_name` (empty → `None`,
/// except the local node which falls back to the hostname) and `last_seen_at`.
/// The local node is online by definition, so it always reports the current
/// time regardless of what its own `sync_nodes` row says.
pub(super) struct NodePresentation {
    pub display_name: Option<String>,
    pub last_seen_at: Option<String>,
}

pub(super) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Batched node name/liveness lookup applying the local-node rule above.
/// Every requested id is present in the result (unknown → both `None`).
pub(super) fn resolve_nodes(
    ctx: &HandlerContext,
    ids: &[String],
) -> Result<HashMap<String, NodePresentation>, anyhow::Error> {
    let rows = repository::lookup_sync_node_info(&ctx.state.db, ids)?;
    let local_id: &str = &ctx.state.local_node_id;
    let mut out = HashMap::with_capacity(ids.len());
    for id in ids {
        let row = rows.get(id);
        let is_local = id == local_id;
        let mut display_name = row
            .map(|(name, _)| name.clone())
            .filter(|name| !name.is_empty());
        if display_name.is_none() && is_local {
            display_name = Some(crate::mesh::node_info_collector::local_hostname());
        }
        let last_seen_at = if is_local {
            Some(now_rfc3339())
        } else {
            row.and_then(|(_, seen)| seen.clone())
        };
        out.insert(
            id.clone(),
            NodePresentation {
                display_name,
                last_seen_at,
            },
        );
    }
    Ok(out)
}

/// Display name + subtitle of a user per D3: `display_name` → `username` →
/// `email`; subtitle `email` → `username`.
pub(super) fn user_presentation(row: &repository::UserNameRow) -> (String, String) {
    let name = [&row.display_name, &row.username, &row.email]
        .into_iter()
        .find(|v| !v.is_empty())
        .cloned()
        .unwrap_or_default();
    let subtitle = if row.email.is_empty() {
        row.username.clone()
    } else {
        row.email.clone()
    };
    (name, subtitle)
}

/// Fills `display_name`/`subtitle`/`member_count`/`last_seen_at` of summary rows
/// according to the `group_by` dimension. Unknown keys keep `None`.
fn decorate_summary_rows(
    ctx: &HandlerContext,
    group_by: &str,
    rows: &mut [ModelMetricsRowWire],
) -> Result<(), anyhow::Error> {
    let keys: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
    match group_by {
        "user" => {
            let users = repository::lookup_user_names(&ctx.state.db, &keys)?;
            for row in rows.iter_mut() {
                if let Some(u) = users.get(&row.key) {
                    let (name, subtitle) = user_presentation(u);
                    row.display_name = Some(name);
                    row.subtitle = Some(subtitle);
                }
            }
        }
        "group" => {
            let groups = repository::lookup_group_info(&ctx.state.db, &keys)?;
            for row in rows.iter_mut() {
                if let Some((name, members)) = groups.get(&row.key) {
                    row.display_name = Some(name.clone());
                    row.member_count = Some(*members);
                }
            }
        }
        "node" => {
            let nodes = resolve_nodes(ctx, &keys)?;
            for row in rows.iter_mut() {
                if let Some(n) = nodes.get(&row.key) {
                    row.display_name = n.display_name.clone();
                    row.last_seen_at = n.last_seen_at.clone();
                }
            }
        }
        "model" => {
            let models = repository::lookup_model_display_names(&ctx.state.db, &keys)?;
            for row in rows.iter_mut() {
                row.display_name = models.get(&row.key).cloned();
            }
        }
        _ => {}
    }
    Ok(())
}

/// Zamienia `period`+`period_key` na inkluzywne granice `hour_bucket` (format
/// `YYYY-MM-DDTHH:00:00Z`). Porównanie leksykograficzne działa, bo bucket ma
/// stałą szerokość RFC3339.
pub(super) fn period_window(
    period: &str,
    period_key: &str,
) -> Result<(String, String), ProtocolError> {
    match period {
        "hourly" => Ok((
            format!("{period_key}:00:00Z"),
            format!("{period_key}:00:00Z"),
        )),
        "daily" => Ok((
            format!("{period_key}T00:00:00Z"),
            format!("{period_key}T23:00:00Z"),
        )),
        "monthly" => Ok((
            format!("{period_key}-01T00:00:00Z"),
            format!("{period_key}-31T23:00:00Z"),
        )),
        other => Err(ProtocolError::bad_request(format!(
            "unknown period '{other}' (expected daily|monthly|hourly)"
        ))),
    }
}

/// Dimension filter that cannot be expressed in SQL (`ModelMetricsFilter` covers
/// only model/user/hours) — node/service/backend/modality/group are sieved
/// in memory. `group_users` = members of `filter.group` (an empty set when the
/// group does not exist or has no members → no row passes).
fn row_matches_filter(
    row: &DbModelMetricsRollup,
    filter: &ModelMetricsFilterWire,
    group_users: Option<&HashSet<String>>,
) -> bool {
    if let Some(users) = group_users {
        if !users.contains(&row.user_id) {
            return false;
        }
    }
    if let Some(node) = &filter.node {
        if &row.node_id != node {
            return false;
        }
    }
    if let Some(service) = &filter.service {
        if &row.service_key != service {
            return false;
        }
    }
    if let Some(backend) = &filter.backend {
        if &row.backend != backend {
            return false;
        }
    }
    if let Some(modality) = &filter.modality {
        if &row.modality != modality {
            return false;
        }
    }
    true
}

/// Koszt jednego wiersza rollupu wg cennika modelu (0 gdy brak cennika).
fn row_cost(row: &DbModelMetricsRollup, pricing: Option<&DbModelPricing>) -> f64 {
    let Some(p) = pricing else {
        return 0.0;
    };
    (row.prompt_tokens as f64 / 1000.0) * p.prompt_per_1k
        + (row.completion_tokens as f64 / 1000.0) * p.completion_per_1k
        + (row.audio_ms as f64 / 60_000.0) * p.audio_per_min
        + (row.images as f64) * p.image_each
        + (row.embedding_tokens as f64 / 1000.0) * p.embedding_per_1k
}

/// Czy wiersz niesie jakies rozliczalne uzycie (tokeny/audio/obrazy). Brak
/// cennika ma znaczenie tylko dla takich wierszy — model bez uzycia nie zaklamie
/// kosztu, wiec nie oznaczamy go jako `missing_pricing`.
fn row_is_billable(row: &DbModelMetricsRollup) -> bool {
    row.prompt_tokens > 0
        || row.completion_tokens > 0
        || row.audio_ms > 0
        || row.images > 0
        || row.embedding_tokens > 0
}

/// Waliduje pojedyncza wartosc cennika: musi byc skonczona i nieujemna, inaczej
/// koszt zsumowany po wielu wierszach da NaN/Inf i zepsuje agregacje.
fn validate_pricing_value(label: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() {
        return Err(format!("{label} must be a finite number"));
    }
    if value < 0.0 {
        return Err(format!("{label} must be >= 0"));
    }
    Ok(())
}

#[handler(variant = "ModelMetricsBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn model_metrics_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelMetricsBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected ModelMetricsBody")),
    };

    match payload {
        ModelMetricsPayload::SummaryRequest {
            period,
            period_key,
            group_by,
            filter,
        } => summary_v1(ctx, period, period_key, group_by, filter),
        ModelMetricsPayload::NodeServiceRequest { period, period_key } => {
            node_service_v1(ctx, period, period_key)
        }
        ModelMetricsPayload::PricingGet => pricing_list_v1(ctx),
        ModelMetricsPayload::PricingSet {
            model_id,
            prompt_per_1k,
            completion_per_1k,
            audio_per_min,
            image_each,
            embedding_per_1k,
        } => pricing_set_v1(
            ctx,
            model_id,
            *prompt_per_1k,
            *completion_per_1k,
            *audio_per_min,
            *image_each,
            *embedding_per_1k,
        ),
        ModelMetricsPayload::SummaryResponse { .. }
        | ModelMetricsPayload::NodeServiceResponse { .. }
        | ModelMetricsPayload::PricingList { .. }
        | ModelMetricsPayload::PricingSetResult { .. } => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_model_metrics_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_model_metrics_dispatch,
            }
        }
    };
}

register_model_metrics_variant!(
    "ModelMetricsSummaryRequest",
    "tentaflow_ws_handler_model_metrics_summary"
);
register_model_metrics_variant!(
    "ModelMetricsNodeServiceRequest",
    "tentaflow_ws_handler_model_metrics_node_service"
);
register_model_metrics_variant!(
    "ModelMetricsPricingGet",
    "tentaflow_ws_handler_model_metrics_pricing_get"
);
register_model_metrics_variant!(
    "ModelMetricsPricingSet",
    "tentaflow_ws_handler_model_metrics_pricing_set"
);

/// Cache cennika per-model w obrębie jednego requestu (jeden lookup na model).
struct PricingCache<'a> {
    pool: &'a crate::db::DbPool,
    org_id: &'a str,
    cache: HashMap<String, Option<DbModelPricing>>,
}

impl<'a> PricingCache<'a> {
    fn new(pool: &'a crate::db::DbPool, org_id: &'a str) -> Self {
        Self {
            pool,
            org_id,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, model_id: &str) -> Option<&DbModelPricing> {
        if !self.cache.contains_key(model_id) {
            let pricing =
                repository::get_model_pricing(self.pool, self.org_id, model_id).unwrap_or(None);
            self.cache.insert(model_id.to_string(), pricing);
        }
        self.cache.get(model_id).and_then(|o| o.as_ref())
    }
}

fn summary_v1(
    ctx: &HandlerContext,
    period: &str,
    period_key: &str,
    group_by: &str,
    filter: &ModelMetricsFilterWire,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    if !matches!(
        group_by,
        "user" | "group" | "model" | "node" | "service" | "day" | "hour"
    ) {
        return Err(ProtocolError::bad_request(format!(
            "unknown group_by '{group_by}' (expected user|group|model|node|service|day|hour)"
        )));
    }
    let (hour_from, hour_to) = period_window(period, period_key)?;
    let db_filter = ModelMetricsFilter {
        model_id: filter.model.as_deref(),
        user_id: filter.user.as_deref(),
        hour_from: Some(&hour_from),
        hour_to: Some(&hour_to),
    };
    let rows = repository::list_model_metrics_rollup(&ctx.state.db, &org.org_id, &db_filter)
        .map_err(|e| db_error("summary", e))?;

    // user_id -> group ids map, only when grouping or filtering by group.
    let group_map: HashMap<String, Vec<String>> = if group_by == "group" || filter.group.is_some() {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (user_id, group_id) in repository::list_group_memberships(&ctx.state.db)
            .map_err(|e| db_error("group_memberships", e))?
        {
            m.entry(user_id).or_default().push(group_id);
        }
        m
    } else {
        HashMap::new()
    };
    let group_users: Option<HashSet<String>> = filter.group.as_ref().map(|group_id| {
        group_map
            .iter()
            .filter(|(_, groups)| groups.contains(group_id))
            .map(|(user_id, _)| user_id.clone())
            .collect()
    });

    let mut pricing = PricingCache::new(&ctx.state.db, &org.org_id);
    let mut groups: HashMap<String, SummaryAgg> = HashMap::new();
    // Dla group_by=group wiersze grup moga sie nakladac (user w kilku grupach) —
    // zbieramy osobna, rozlaczna sume (kazdy wiersz policzony raz).
    let mut grand: Option<SummaryAgg> = (group_by == "group").then(SummaryAgg::default);
    for row in &rows {
        if !row_matches_filter(row, filter, group_users.as_ref()) {
            continue;
        }
        let pricing_row = pricing.get(&row.model_id);
        let missing_pricing = pricing_row.is_none() && row_is_billable(row);
        let cost = row_cost(row, pricing_row);
        if let Some(g) = grand.as_mut() {
            g.add_row(row, cost, missing_pricing);
        }
        let keys: Vec<String> = match group_by {
            "user" => vec![row.user_id.clone()],
            "model" => vec![row.model_id.clone()],
            "node" => vec![row.node_id.clone()],
            "service" => vec![row.service_key.clone()],
            "day" => vec![row.hour_bucket.chars().take(10).collect()],
            "hour" => vec![row.hour_bucket.clone()],
            // Only "group" is left after the up-front group_by validation.
            _ => match group_map.get(&row.user_id) {
                Some(ids) if !ids.is_empty() => ids.clone(),
                _ => vec![NO_GROUP_KEY.to_string()],
            },
        };
        for key in keys {
            groups
                .entry(key)
                .or_default()
                .add_row(row, cost, missing_pricing);
        }
    }

    let mut wire: Vec<ModelMetricsRowWire> = groups
        .into_iter()
        .map(|(key, agg)| agg.into_wire(key))
        .collect();
    wire.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens).then(a.key.cmp(&b.key)));
    decorate_summary_rows(ctx, group_by, &mut wire).map_err(|e| db_error("summary_names", e))?;
    let grand_total = grand.map(|agg| agg.into_wire("__grand_total__".to_string()));
    Ok(MessageBody::ModelMetricsBody(
        ModelMetricsPayload::SummaryResponse {
            rows: wire,
            grand_total,
        },
    ))
}

/// Akumulator jednej pary węzeł×serwis.
#[derive(Default)]
struct NodeServiceAgg {
    backend: String,
    model_id: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    request_count: i64,
    success_count: i64,
    error_count: i64,
    ttft: [i64; 10],
    ttft_samples: i64,
    decode: [i64; 8],
    decode_samples: i64,
}

fn node_service_v1(
    ctx: &HandlerContext,
    period: &str,
    period_key: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (hour_from, hour_to) = period_window(period, period_key)?;
    let db_filter = ModelMetricsFilter {
        model_id: None,
        user_id: None,
        hour_from: Some(&hour_from),
        hour_to: Some(&hour_to),
    };
    let rows = repository::list_model_metrics_rollup(&ctx.state.db, &org.org_id, &db_filter)
        .map_err(|e| db_error("node_service", e))?;

    let ttft_edges = ttft_edges_f64();
    let mut groups: HashMap<(String, String), NodeServiceAgg> = HashMap::new();
    for row in &rows {
        let agg = groups
            .entry((row.node_id.clone(), row.service_key.clone()))
            .or_default();
        // backend/model reprezentatywny — ostatni wygrywa (spójny w obrębie serwisu).
        agg.backend = row.backend.clone();
        agg.model_id = row.model_id.clone();
        agg.prompt_tokens += row.prompt_tokens;
        agg.completion_tokens += row.completion_tokens;
        agg.total_tokens += row.total_tokens;
        agg.request_count += row.request_count;
        agg.success_count += row.success_count;
        agg.error_count += row.error_count;
        if row.histogram_version == MODEL_METRICS_HISTOGRAM_VERSION {
            for i in 0..10 {
                agg.ttft[i] += row.ttft_buckets[i];
            }
            for i in 0..8 {
                agg.decode[i] += row.decode_tps_buckets[i];
            }
            agg.ttft_samples += row.ttft_sample_count;
            agg.decode_samples += row.decode_tps_sample_count;
        }
    }

    let mut wire: Vec<ModelNodeServiceRowWire> = groups
        .into_iter()
        .map(|((node_id, service_key), agg)| {
            let error_rate = if agg.request_count > 0 {
                agg.error_count as f64 / agg.request_count as f64
            } else {
                0.0
            };
            ModelNodeServiceRowWire {
                node_id,
                service_key,
                backend: agg.backend,
                model_id: agg.model_id,
                prompt_tokens: agg.prompt_tokens,
                completion_tokens: agg.completion_tokens,
                total_tokens: agg.total_tokens,
                request_count: agg.request_count,
                success_count: agg.success_count,
                error_count: agg.error_count,
                error_rate,
                ttft_p50: percentile_from_histogram(&agg.ttft, &ttft_edges, agg.ttft_samples, 50.0),
                ttft_p90: percentile_from_histogram(&agg.ttft, &ttft_edges, agg.ttft_samples, 90.0),
                ttft_p99: percentile_from_histogram(&agg.ttft, &ttft_edges, agg.ttft_samples, 99.0),
                decode_p50: percentile_from_histogram(
                    &agg.decode,
                    &DECODE_TPS_EDGES,
                    agg.decode_samples,
                    50.0,
                ),
                decode_p90: percentile_from_histogram(
                    &agg.decode,
                    &DECODE_TPS_EDGES,
                    agg.decode_samples,
                    90.0,
                ),
                decode_p99: percentile_from_histogram(
                    &agg.decode,
                    &DECODE_TPS_EDGES,
                    agg.decode_samples,
                    99.0,
                ),
                node_display_name: None,
                node_last_seen_at: None,
                model_display_name: None,
            }
        })
        .collect();
    wire.sort_by(|a, b| {
        b.total_tokens
            .cmp(&a.total_tokens)
            .then(a.node_id.cmp(&b.node_id))
            .then(a.service_key.cmp(&b.service_key))
    });
    let node_ids: Vec<String> = wire
        .iter()
        .map(|r| r.node_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let model_ids: Vec<String> = wire
        .iter()
        .map(|r| r.model_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let nodes = resolve_nodes(ctx, &node_ids).map_err(|e| db_error("node_service_nodes", e))?;
    let models = repository::lookup_model_display_names(&ctx.state.db, &model_ids)
        .map_err(|e| db_error("node_service_models", e))?;
    for row in wire.iter_mut() {
        if let Some(n) = nodes.get(&row.node_id) {
            row.node_display_name = n.display_name.clone();
            row.node_last_seen_at = n.last_seen_at.clone();
        }
        row.model_display_name = models.get(&row.model_id).cloned();
    }
    Ok(MessageBody::ModelMetricsBody(
        ModelMetricsPayload::NodeServiceResponse { rows: wire },
    ))
}

fn pricing_to_wire(p: DbModelPricing) -> ModelPricingWire {
    ModelPricingWire {
        model_id: p.model_id,
        prompt_per_1k: p.prompt_per_1k,
        completion_per_1k: p.completion_per_1k,
        audio_per_min: p.audio_per_min,
        image_each: p.image_each,
        updated_at: p.updated_at,
        embedding_per_1k: p.embedding_per_1k,
    }
}

fn pricing_list_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let rows = repository::list_model_pricing(&ctx.state.db, &org.org_id)
        .map_err(|e| db_error("pricing_list", e))?
        .into_iter()
        .map(pricing_to_wire)
        .collect();
    Ok(MessageBody::ModelMetricsBody(
        ModelMetricsPayload::PricingList { rows },
    ))
}

fn pricing_set_v1(
    ctx: &HandlerContext,
    model_id: &str,
    prompt_per_1k: f64,
    completion_per_1k: f64,
    audio_per_min: f64,
    image_each: f64,
    embedding_per_1k: f64,
) -> Result<MessageBody, ProtocolError> {
    let org = require_write(ctx)?;
    let validation = validate_pricing_value("prompt_per_1k", prompt_per_1k)
        .and_then(|()| validate_pricing_value("completion_per_1k", completion_per_1k))
        .and_then(|()| validate_pricing_value("audio_per_min", audio_per_min))
        .and_then(|()| validate_pricing_value("image_each", image_each))
        .and_then(|()| validate_pricing_value("embedding_per_1k", embedding_per_1k));
    if let Err(error) = validation {
        return Ok(MessageBody::ModelMetricsBody(
            ModelMetricsPayload::PricingSetResult {
                ok: false,
                error: Some(error),
            },
        ));
    }
    repository::upsert_model_pricing(
        &ctx.state.db,
        &NewModelPricing {
            model_id,
            org_id: &org.org_id,
            prompt_per_1k,
            completion_per_1k,
            audio_per_min,
            image_each,
            embedding_per_1k,
        },
    )
    .map_err(|e| db_error("pricing_set", e))?;
    Ok(MessageBody::ModelMetricsBody(
        ModelMetricsPayload::PricingSetResult {
            ok: true,
            error: None,
        },
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::models::{
        ModelMetricsCounters, ModelMetricsDims, ModelMetricsPerfSamples, ModelMetricsTimes,
        ModelMetricsTokens,
    };
    use crate::dispatch::state::AppState;
    use crate::services::org::DEFAULT_ORG_ID;
    use std::collections::HashSet;
    use tentaflow_protocol::SessionAuth;

    const REMOTE_NODE: &str = "7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d";
    /// Known only to `peer_persisted` (mesh heartbeat), no `sync_nodes` row.
    const PEER_ONLY_NODE: &str = "33f1c904a7b2e5d8c1f4a7b0e3d6c9f2a5b8e1d4c7f0a3b6e9d2c5f8a1b4e7d0";

    /// Writes a peer heartbeat row like the peer registry persistence writer.
    pub(crate) fn seed_peer_heartbeat(ctx: &HandlerContext, node_hex: &str, last_seen_ms: i64) {
        let mut id = [0u8; 32];
        hex::decode_to_slice(node_hex, &mut id).unwrap();
        repository::upsert_peer_persisted_batch(
            &ctx.state.db,
            &[repository::PeerPersistedRow {
                node_id: id,
                pubkey: vec![1, 2, 3],
                trust_state: 0,
                hostname: None,
                platform: None,
                role: 0,
                last_seen_ms,
                persisted_ver: 1,
                updated_at_ms: last_seen_ms,
            }],
        )
        .unwrap();
    }

    pub(crate) fn reader_ctx() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0u8; 16],
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: AppState::for_test(),
            org_context: Some(OrgContext {
                user_id: "admin".to_string(),
                org_id: DEFAULT_ORG_ID.to_string(),
                role_id: "role-org-admin".to_string(),
                permissions: ["metrics.read", "tokens.read"]
                    .into_iter()
                    .map(str::to_string)
                    .collect::<HashSet<_>>(),
            }),
        }
    }

    /// Users u1 (name+email), u2 (username only), group g1 = {u1, u2}, a remote
    /// sync node with a display name, and a Qwen catalog entry.
    pub(crate) fn seed_directory(ctx: &HandlerContext) {
        let conn = ctx.state.db.write().unwrap();
        conn.execute_batch(
            "INSERT INTO user_accounts (id, username, password_hash, display_name, email) \
             VALUES ('u1', 'marta.k', 'x', 'Marta Kowalczyk', 'marta.k@firma.pl'); \
             INSERT INTO user_accounts (id, username, password_hash) VALUES ('u2', 'piotr.w', 'x'); \
             INSERT INTO user_groups (id, name) VALUES ('g1', 'Marketing'); \
             INSERT INTO group_members (group_id, user_id) VALUES ('g1', 'u1'), ('g1', 'u2'); \
             INSERT INTO sync_nodes (node_id, public_key, display_name, last_seen_at) \
             VALUES ('7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', 'pk', \
                     'biuro-mini', '2026-08-19T10:00:00Z'); \
             INSERT INTO services (id, engine_id, category, display_name, deploy_method, transport, status) \
             VALUES (9001, 'vllm', 'llm', 'vLLM', 'external', 'external_http', 'stopped'); \
             INSERT INTO model_registry (service_id, model_name, display_name) \
             VALUES (9001, 'qwen', 'Qwen 3.8 27B AWQ');",
        )
        .unwrap();
    }

    pub(crate) fn bump(
        ctx: &HandlerContext,
        node: &str,
        user: &str,
        model: &str,
        hour: &str,
        tokens: i64,
    ) {
        repository::bump_model_metrics_rollup(
            &ctx.state.db,
            &ModelMetricsDims {
                node_id: node,
                org_id: DEFAULT_ORG_ID,
                user_id: user,
                model_id: model,
                service_key: "vllm:qwen",
                backend: "vllm",
                modality: "chat",
                hour_bucket: hour,
                histogram_version: MODEL_METRICS_HISTOGRAM_VERSION,
            },
            &ModelMetricsCounters {
                request_count: 1,
                success_count: 1,
                error_count: 0,
                usage_missing_count: 0,
            },
            &ModelMetricsTokens {
                prompt_tokens: tokens / 2,
                completion_tokens: tokens - tokens / 2,
                total_tokens: tokens,
                ..Default::default()
            },
            &ModelMetricsTimes::default(),
            &ModelMetricsPerfSamples {
                ttft_ms: Some(120),
                decode_tps: Some(50.0),
                e2e_ms: Some(900),
            },
        )
        .unwrap();
    }

    fn summary_rows(
        ctx: &HandlerContext,
        group_by: &str,
        filter: ModelMetricsFilterWire,
    ) -> Vec<ModelMetricsRowWire> {
        match summary_v1(ctx, "monthly", "2026-08", group_by, &filter).unwrap() {
            MessageBody::ModelMetricsBody(ModelMetricsPayload::SummaryResponse {
                rows, ..
            }) => rows,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn percentile_interpolates_inside_bucket() {
        let edges = ttft_edges_f64();
        let mut counts = [0i64; 10];
        counts[2] = 10; // (50, 100]
        assert_eq!(
            percentile_from_histogram(&counts, &edges, 10, 50.0),
            Some(75.0)
        );
        assert_eq!(percentile_from_histogram(&counts, &edges, 0, 50.0), None);
    }

    #[test]
    fn summary_resolves_user_group_node_and_model_names() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        let local = ctx.state.local_node_id.to_string();
        bump(&ctx, &local, "u1", "qwen", "2026-08-19T10:00:00Z", 1000);
        bump(&ctx, REMOTE_NODE, "u2", "qwen", "2026-08-19T11:00:00Z", 400);
        bump(&ctx, REMOTE_NODE, "u3", "other", "2026-08-19T11:00:00Z", 50);

        let users = summary_rows(&ctx, "user", ModelMetricsFilterWire::default());
        let u1 = users.iter().find(|r| r.key == "u1").unwrap();
        assert_eq!(u1.display_name.as_deref(), Some("Marta Kowalczyk"));
        assert_eq!(u1.subtitle.as_deref(), Some("marta.k@firma.pl"));
        let u2 = users.iter().find(|r| r.key == "u2").unwrap();
        assert_eq!(u2.display_name.as_deref(), Some("piotr.w"));
        assert_eq!(u2.subtitle.as_deref(), Some("piotr.w"));
        let u3 = users.iter().find(|r| r.key == "u3").unwrap();
        assert_eq!(u3.display_name, None);

        let groups = summary_rows(&ctx, "group", ModelMetricsFilterWire::default());
        let g1 = groups.iter().find(|r| r.key == "g1").unwrap();
        assert_eq!(g1.display_name.as_deref(), Some("Marketing"));
        assert_eq!(g1.member_count, Some(2));
        assert_eq!(g1.total_tokens, 1400);
        let none = groups.iter().find(|r| r.key == NO_GROUP_KEY).unwrap();
        assert_eq!(none.total_tokens, 50);
        assert_eq!(none.display_name, None);

        let nodes = summary_rows(&ctx, "node", ModelMetricsFilterWire::default());
        let local_row = nodes.iter().find(|r| r.key == local).unwrap();
        assert_eq!(
            local_row.display_name.as_deref(),
            Some(crate::mesh::node_info_collector::local_hostname().as_str())
        );
        assert!(local_row.last_seen_at.is_some(), "local node is online now");
        let remote = nodes.iter().find(|r| r.key == REMOTE_NODE).unwrap();
        assert_eq!(remote.display_name.as_deref(), Some("biuro-mini"));
        assert_eq!(remote.last_seen_at.as_deref(), Some("2026-08-19T10:00:00Z"));

        let models = summary_rows(&ctx, "model", ModelMetricsFilterWire::default());
        let qwen = models.iter().find(|r| r.key == "qwen").unwrap();
        assert_eq!(qwen.display_name.as_deref(), Some("Qwen 3.8 27B AWQ"));
        assert_eq!(
            models
                .iter()
                .find(|r| r.key == "other")
                .unwrap()
                .display_name,
            None
        );

        let hours = summary_rows(&ctx, "hour", ModelMetricsFilterWire::default());
        assert_eq!(hours.len(), 2);
        assert!(hours
            .iter()
            .any(|r| r.key == "2026-08-19T10:00:00Z" && r.total_tokens == 1000));
    }

    #[test]
    fn summary_filters_by_user_and_group() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        let local = ctx.state.local_node_id.to_string();
        bump(&ctx, &local, "u1", "qwen", "2026-08-19T10:00:00Z", 1000);
        bump(&ctx, &local, "u2", "qwen", "2026-08-19T10:00:00Z", 400);
        bump(&ctx, &local, "u3", "qwen", "2026-08-19T10:00:00Z", 50);

        let by_user = summary_rows(
            &ctx,
            "model",
            ModelMetricsFilterWire {
                user: Some("u2".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(by_user.len(), 1);
        assert_eq!(by_user[0].total_tokens, 400);

        let by_group = summary_rows(
            &ctx,
            "user",
            ModelMetricsFilterWire {
                group: Some("g1".to_string()),
                ..Default::default()
            },
        );
        let keys: HashSet<&str> = by_group.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, HashSet::from(["u1", "u2"]));

        let unknown_group = summary_rows(
            &ctx,
            "user",
            ModelMetricsFilterWire {
                group: Some("missing".to_string()),
                ..Default::default()
            },
        );
        assert!(unknown_group.is_empty());

        let err = summary_v1(
            &ctx,
            "monthly",
            "2026-08",
            "week",
            &ModelMetricsFilterWire::default(),
        )
        .unwrap_err();
        assert!(err.message.contains("unknown group_by"));
    }

    #[test]
    fn node_liveness_merges_peer_heartbeats() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        let local = ctx.state.local_node_id.to_string();
        // 2026-08-19T12:30:00Z — later than the seeded sync_nodes value (10:00).
        seed_peer_heartbeat(&ctx, REMOTE_NODE, 1_787_142_600_000);
        // 2026-08-19T11:00:00Z for a node without any sync_nodes row.
        seed_peer_heartbeat(&ctx, PEER_ONLY_NODE, 1_787_137_200_000);
        bump(&ctx, REMOTE_NODE, "u1", "qwen", "2026-08-19T10:00:00Z", 10);
        bump(
            &ctx,
            PEER_ONLY_NODE,
            "u1",
            "qwen",
            "2026-08-19T10:00:00Z",
            10,
        );
        bump(&ctx, &local, "u1", "qwen", "2026-08-19T10:00:00Z", 10);

        let nodes = summary_rows(&ctx, "node", ModelMetricsFilterWire::default());
        let remote = nodes.iter().find(|r| r.key == REMOTE_NODE).unwrap();
        assert_eq!(remote.display_name.as_deref(), Some("biuro-mini"));
        assert_eq!(
            remote.last_seen_at.as_deref(),
            Some("2026-08-19T12:30:00Z"),
            "heartbeat newer than sync_nodes wins"
        );
        let peer_only = nodes.iter().find(|r| r.key == PEER_ONLY_NODE).unwrap();
        assert_eq!(peer_only.display_name, None);
        assert_eq!(
            peer_only.last_seen_at.as_deref(),
            Some("2026-08-19T11:00:00Z")
        );

        // An OLDER heartbeat must not move the resolved liveness backwards.
        let stale =
            repository::lookup_sync_node_info(&ctx.state.db, &[REMOTE_NODE.to_string()]).unwrap();
        assert_eq!(
            stale[REMOTE_NODE].1.as_deref(),
            Some("2026-08-19T12:30:00Z")
        );
    }

    #[test]
    fn node_service_rows_carry_node_and_model_names() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        bump(
            &ctx,
            REMOTE_NODE,
            "u1",
            "qwen",
            "2026-08-19T10:00:00Z",
            1000,
        );
        let rows = match node_service_v1(&ctx, "daily", "2026-08-19").unwrap() {
            MessageBody::ModelMetricsBody(ModelMetricsPayload::NodeServiceResponse { rows }) => {
                rows
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node_display_name.as_deref(), Some("biuro-mini"));
        assert_eq!(
            rows[0].node_last_seen_at.as_deref(),
            Some("2026-08-19T10:00:00Z")
        );
        assert_eq!(
            rows[0].model_display_name.as_deref(),
            Some("Qwen 3.8 27B AWQ")
        );
    }
}

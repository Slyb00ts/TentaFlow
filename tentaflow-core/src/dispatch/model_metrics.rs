// =============================================================================
// Plik: dispatch/model_metrics.rs
// Opis: Handlery binarnego API metryk modeli — zagregowane summary (histogramy
//       TTFT/decode/e2e), przekrój węzeł×serwis oraz cennik per-model. Wszystko
//       po CBOR. Agregacja mesh-wide wynika z replikowanego `model_metrics_rollup`.
// Przykład: ModelMetricsPayload::SummaryRequest zwraca zsumowane metryki grupy.
// =============================================================================

use std::collections::HashMap;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ModelMetricsFilterWire, ModelMetricsPayload, ModelMetricsRowWire,
    ModelNodeServiceRowWire, ModelPricingWire, ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::db::models::{DbModelMetricsRollup, DbModelPricing, ModelMetricsFilter, NewModelPricing};
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
        }
    }
}

/// Zamienia `period`+`period_key` na inkluzywne granice `hour_bucket` (format
/// `YYYY-MM-DDTHH:00:00Z`). Porównanie leksykograficzne działa, bo bucket ma
/// stałą szerokość RFC3339.
fn period_window(period: &str, period_key: &str) -> Result<(String, String), ProtocolError> {
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

/// Filtr wymiarów, który nie da się wyrazić w SQL (`ModelMetricsFilter` pokrywa
/// tylko model/user/godziny) — node/service/backend/modality odsiewamy w pamięci.
fn row_matches_filter(row: &DbModelMetricsRollup, filter: &ModelMetricsFilterWire) -> bool {
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
}

/// Czy wiersz niesie jakies rozliczalne uzycie (tokeny/audio/obrazy). Brak
/// cennika ma znaczenie tylko dla takich wierszy — model bez uzycia nie zaklamie
/// kosztu, wiec nie oznaczamy go jako `missing_pricing`.
fn row_is_billable(row: &DbModelMetricsRollup) -> bool {
    row.prompt_tokens > 0 || row.completion_tokens > 0 || row.audio_ms > 0 || row.images > 0
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
        } => pricing_set_v1(
            ctx,
            model_id,
            *prompt_per_1k,
            *completion_per_1k,
            *audio_per_min,
            *image_each,
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
            let pricing = repository::get_model_pricing(self.pool, self.org_id, model_id)
                .unwrap_or(None);
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
    let (hour_from, hour_to) = period_window(period, period_key)?;
    let db_filter = ModelMetricsFilter {
        model_id: filter.model.as_deref(),
        user_id: None,
        hour_from: Some(&hour_from),
        hour_to: Some(&hour_to),
    };
    let rows = repository::list_model_metrics_rollup(&ctx.state.db, &org.org_id, &db_filter)
        .map_err(|e| db_error("summary", e))?;

    // Mapa user_id -> grupy tylko gdy grupujemy po grupie.
    let group_map: HashMap<String, Vec<String>> = if group_by == "group" {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (user_id, group_name) in repository::list_group_memberships(&ctx.state.db)
            .map_err(|e| db_error("group_memberships", e))?
        {
            m.entry(user_id).or_default().push(group_name);
        }
        m
    } else {
        HashMap::new()
    };

    let mut pricing = PricingCache::new(&ctx.state.db, &org.org_id);
    let mut groups: HashMap<String, SummaryAgg> = HashMap::new();
    // Dla group_by=group wiersze grup moga sie nakladac (user w kilku grupach) —
    // zbieramy osobna, rozlaczna sume (kazdy wiersz policzony raz).
    let mut grand: Option<SummaryAgg> = (group_by == "group").then(SummaryAgg::default);
    for row in &rows {
        if !row_matches_filter(row, filter) {
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
            "group" => match group_map.get(&row.user_id) {
                Some(names) if !names.is_empty() => names.clone(),
                _ => vec![NO_GROUP_KEY.to_string()],
            },
            other => {
                return Err(ProtocolError::bad_request(format!(
                    "unknown group_by '{other}' (expected user|group|model|node|service|day)"
                )));
            }
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
            }
        })
        .collect();
    wire.sort_by(|a, b| {
        b.total_tokens
            .cmp(&a.total_tokens)
            .then(a.node_id.cmp(&b.node_id))
            .then(a.service_key.cmp(&b.service_key))
    });
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
) -> Result<MessageBody, ProtocolError> {
    let org = require_write(ctx)?;
    let validation = validate_pricing_value("prompt_per_1k", prompt_per_1k)
        .and_then(|()| validate_pricing_value("completion_per_1k", completion_per_1k))
        .and_then(|()| validate_pricing_value("audio_per_min", audio_per_min))
        .and_then(|()| validate_pricing_value("image_each", image_each));
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

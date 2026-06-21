// =============================================================================
// Plik: flow_engine/node_adapters/vector.rs
// Opis: VectorNodeAdapter (NODE_TYPE="vector") — węzeł flow upsert/search/hybrid
//       nad `NamespaceManager` z `ctx.vectors`, scoped do (org, addon_instance,
//       namespace). Tożsamość instancji bierze z `ctx.addon_id` (RAG E1.0); org z
//       `ctx.org_id` (fallback DEFAULT_ORG_ID tylko gdy None). Bez `addon_id` →
//       czytelny błąd zamiast zapisu w cudzą/domyślną przestrzeń.
// Przykład: node config {"op":"search","namespace":"passages","dim":4,"top_k":10}
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::org::DEFAULT_ORG_ID;
use crate::services::vector::backend::{Field, FieldSpec, Fusion, Metric, SparseVector};
use tentaflow_sdk_spec::{FieldType, FieldValue, Filter};

const NODE_TYPE: &str = "vector";

/// Twardy cap na `top_k` — lustro `MAX_SEARCH_K` z host-fn wektorowych. `top_k`
/// ponad to jest odrzucane (a nie cicho capowane), żeby flow nie maskował błędu
/// konfiguracji.
const MAX_TOP_K: u32 = 1000;

/// Bezpieczna konwersja JSON-owego ref_id na `u64`. JSON number > i64::MAX nie
/// panikuje (bug 1 z review codex: `is_u64()` + `as_i64().unwrap()`), zwraca
/// błąd z kontekstem. `ref_id == 0` odrzucamy TU, w fazie 1 walidacji — backend
/// zvec też je odrzuca, ale dopiero przy zapisie (faza 2), więc bez tego sprawdzenia
/// zły `ref_id=0` w środku batcha pozwoliłby zapisać wcześniejsze itemy zanim padnie
/// (częściowy zapis łamie inwariant „waliduj wszystko przed jakimkolwiek zapisem").
fn parse_ref_id(v: &serde_json::Value, ctx: &str) -> Result<u64> {
    let id = v
        .as_u64()
        .ok_or_else(|| anyhow!("vector adapter: {ctx}: ref_id musi być nieujemną liczbą całkowitą <= u64::MAX, dostał {v}"))?;
    if id == 0 {
        return Err(anyhow!("vector adapter: {ctx}: ref_id musi być > 0 (0 jest zarezerwowane)"));
    }
    Ok(id)
}

/// Bezpieczna konwersja `u64 → u32` z walidacją zakresu PRZED rzutowaniem (bug 2
/// i 3 z review: `u64 as u32` zawijał np. 4294967297 → 1).
fn u64_to_u32(n: u64, what: &str) -> Result<u32> {
    u32::try_from(n).map_err(|_| {
        anyhow!("vector adapter: {what}={n} poza zakresem u32 (max {})", u32::MAX)
    })
}

pub struct VectorNodeAdapter;

impl VectorNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Org-scope: `ctx.org_id` gdy `Some`, w p.p. `DEFAULT_ORG_ID`. NIE
    /// hardkodujemy DEFAULT_ORG_ID — to fallback tylko dla wywołań bez org.
    fn org_scope(ctx: &ExecutionContext) -> String {
        ctx.org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string())
    }

    /// Tożsamość instancji addona z kontekstu. `None` (wywołanie nie-addonowe)
    /// → błąd: węzeł retrievalu nie wie w którą przestrzeń uderzać, więc odmawia
    /// zamiast zapisać/czytać z błędnej.
    fn addon_scope(ctx: &ExecutionContext) -> Result<&str> {
        ctx.addon_id.as_deref().ok_or_else(|| {
            anyhow!(
                "vector adapter: brak tożsamości addona (ctx.addon_id=None) — węzeł \
                 vector wymaga wywołania flow JAKO MODEL przez addon (RAG E1.0)"
            )
        })
    }

    /// Wybiera `op` z node.config (wymagane). `upsert`|`search`|`hybrid`.
    fn pick_op(node: &FlowNode) -> Result<&str> {
        node.config
            .get("op")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!("vector adapter: brak wymaganego 'op' (upsert|search|hybrid) w node.config")
            })
    }

    /// Namespace z node.config (wymagane).
    fn pick_namespace(node: &FlowNode) -> Result<String> {
        node.config
            .get("namespace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("vector adapter: brak wymaganego 'namespace' w node.config"))
    }

    /// Metric z node.config (domyślnie cosine). Nieznana wartość → błąd.
    fn pick_metric(node: &FlowNode) -> Result<Metric> {
        match node.config.get("metric").and_then(|v| v.as_str()) {
            None => Ok(Metric::Cosine),
            Some(s) => Metric::parse(s)
                .ok_or_else(|| anyhow!("vector adapter: nieznana metryka '{s}' (cosine|euclidean|dot)")),
        }
    }

    /// Parsuje listę typowanych pól metadanych z JSON `[{name,value}]`. Typ
    /// wyprowadzamy z JSON-owej wartości (str/int/float/bool). >i64::MAX dla int
    /// → traktujemy jako float (serde_json zwraca u64), bo `FieldValue::Int` to
    /// i64; zachowanie spójne z host-fn (bez panika).
    fn parse_fields(v: Option<&serde_json::Value>) -> Result<(Vec<FieldSpec>, Vec<Field>)> {
        let Some(arr) = v.and_then(|v| v.as_array()) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let mut specs = Vec::with_capacity(arr.len());
        let mut values = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("vector adapter: field[{i}] brak 'name'"))?
                .to_string();
            let raw = item
                .get("value")
                .ok_or_else(|| anyhow!("vector adapter: field[{i}] brak 'value'"))?;
            let (field_type, value) = json_to_field_value(raw, i)?;
            let indexed = item
                .get("indexed")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            specs.push(FieldSpec {
                name: name.clone(),
                field_type,
                indexed,
            });
            values.push(Field { name, value });
        }
        Ok((specs, values))
    }

    /// Parsuje sparse vector `{indices:[u32], values:[f32]}`. Każdy index
    /// walidowany do zakresu u32 PRZED rzutowaniem (bug 2: `u64 as u32` bez
    /// kontroli zakresu). Długości muszą być równe.
    fn parse_sparse(v: Option<&serde_json::Value>) -> Result<Option<SparseVector>> {
        let Some(obj) = v else {
            return Ok(None);
        };
        let indices_raw = obj
            .get("indices")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("vector adapter: sparse: brak 'indices' (tablica)"))?;
        let values_raw = obj
            .get("values")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("vector adapter: sparse: brak 'values' (tablica)"))?;
        if indices_raw.len() != values_raw.len() {
            return Err(anyhow!(
                "vector adapter: sparse: indices ({}) i values ({}) mają różną długość",
                indices_raw.len(),
                values_raw.len()
            ));
        }
        let mut indices = Vec::with_capacity(indices_raw.len());
        for (i, idx) in indices_raw.iter().enumerate() {
            let n = idx
                .as_u64()
                .ok_or_else(|| anyhow!("vector adapter: sparse.indices[{i}] nie jest liczbą całkowitą >= 0"))?;
            indices.push(u64_to_u32(n, &format!("sparse.indices[{i}]"))?);
        }
        let mut values = Vec::with_capacity(values_raw.len());
        for (i, val) in values_raw.iter().enumerate() {
            let f = val
                .as_f64()
                .ok_or_else(|| anyhow!("vector adapter: sparse.values[{i}] nie jest liczbą"))?;
            values.push(f as f32);
        }
        Ok(Some(SparseVector { indices, values }))
    }

    /// `top_k` z node.config (domyślnie 10). Walidacja zakresu (1..=MAX_TOP_K)
    /// PRZED rzutowaniem na u32 (bug 3: 4294967297 zawijał do 1).
    fn parse_top_k(node: &FlowNode) -> Result<usize> {
        let raw = node.config.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10);
        let k = u64_to_u32(raw, "top_k")?;
        if k == 0 || k > MAX_TOP_K {
            return Err(anyhow!(
                "vector adapter: top_k={k} poza zakresem 1..={MAX_TOP_K}"
            ));
        }
        Ok(k as usize)
    }

    /// Fuzja dla hybrid: `{"rrf": k}` albo `{"weighted":[dense,sparse]}`.
    /// Domyślnie `Rrf(60)`. `rrf` walidowany do zakresu u32 PRZED rzutowaniem
    /// (bug 3: `u64 as u32` przed walidacją).
    fn parse_fusion(node: &FlowNode) -> Result<Fusion> {
        let Some(f) = node.config.get("fusion") else {
            return Ok(Fusion::Rrf(60));
        };
        if let Some(k) = f.get("rrf").and_then(|v| v.as_u64()) {
            return Ok(Fusion::Rrf(u64_to_u32(k, "fusion.rrf")?));
        }
        if let Some(w) = f.get("weighted").and_then(|v| v.as_array()) {
            if w.len() != 2 {
                return Err(anyhow!(
                    "vector adapter: fusion.weighted musi mieć 2 elementy [dense, sparse]"
                ));
            }
            let dense = w[0]
                .as_f64()
                .ok_or_else(|| anyhow!("vector adapter: fusion.weighted[0] nie jest liczbą"))?
                as f32;
            let sparse = w[1]
                .as_f64()
                .ok_or_else(|| anyhow!("vector adapter: fusion.weighted[1] nie jest liczbą"))?
                as f32;
            return Ok(Fusion::Weighted(dense, sparse));
        }
        Err(anyhow!(
            "vector adapter: fusion musi być {{\"rrf\": k}} albo {{\"weighted\": [d, s]}}"
        ))
    }

    /// Wyciąga dense query z payload: `FlowValue::Embedding` albo
    /// `FlowValue::Json{vector|query|embedding:[f32]}`.
    fn query_vector(envelope: &FlowEnvelope) -> Result<Vec<f32>> {
        match &envelope.payload {
            FlowValue::Embedding(v) => Ok(v.clone()),
            FlowValue::Json(obj) => obj
                .get("vector")
                .or_else(|| obj.get("query"))
                .or_else(|| obj.get("embedding"))
                .and_then(|v| v.as_array())
                .map(|arr| json_f32_array(arr))
                .transpose()?
                .ok_or_else(|| {
                    anyhow!("vector adapter: search: payload Json bez 'vector'/'query'/'embedding' (tablica f32)")
                }),
            other => Err(anyhow!(
                "vector adapter: search: payload musi być Embedding albo Json, dostał {}",
                other.kind()
            )),
        }
    }

    /// Op upsert: czyta `items:[{ref_id,vector,fields?,sparse?}]` z payload Json.
    /// WALIDACJA-PRZED-ZAPISEM (bug 4): wszystkie itemy (w tym ref_id, długości,
    /// zakresy) parsujemy i sprawdzamy PRZED jakimkolwiek zapisem do backendu;
    /// dopiero potem upsertujemy w pętli. Zły item w batchu → nic nie zapisane.
    async fn op_upsert(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let namespace = Self::pick_namespace(node)?;
        let metric = Self::pick_metric(node)?;

        let items = match &envelope.payload {
            FlowValue::Json(obj) => obj
                .get("items")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("vector adapter: upsert: payload Json bez 'items' (tablica)"))?,
            other => {
                return Err(anyhow!(
                    "vector adapter: upsert: payload musi być Json{{items:[...]}}, dostał {}",
                    other.kind()
                ))
            }
        };
        if items.is_empty() {
            return Err(anyhow!("vector adapter: upsert: pusta lista 'items'"));
        }

        // Faza 1 — pełna walidacja WSZYSTKICH itemów (zero zapisów).
        struct PreparedItem {
            ref_id: u64,
            vector: Vec<f32>,
            specs: Vec<FieldSpec>,
            values: Vec<Field>,
            sparse: Option<SparseVector>,
        }
        let mut prepared = Vec::with_capacity(items.len());
        let mut dim: Option<u32> = None;
        for (i, item) in items.iter().enumerate() {
            let ref_id = parse_ref_id(
                item.get("ref_id")
                    .ok_or_else(|| anyhow!("vector adapter: item[{i}] brak 'ref_id'"))?,
                &format!("item[{i}]"),
            )?;
            let vector = item
                .get("vector")
                .and_then(|v| v.as_array())
                .map(|arr| json_f32_array(arr))
                .transpose()?
                .ok_or_else(|| anyhow!("vector adapter: item[{i}] brak 'vector' (tablica f32)"))?;
            if vector.is_empty() {
                return Err(anyhow!("vector adapter: item[{i}] 'vector' jest puste"));
            }
            let item_dim = u64_to_u32(vector.len() as u64, &format!("item[{i}] dim"))?;
            match dim {
                None => dim = Some(item_dim),
                Some(d) if d != item_dim => {
                    return Err(anyhow!(
                        "vector adapter: item[{i}] dim {item_dim} != dim {d} wcześniejszych itemów"
                    ))
                }
                Some(_) => {}
            }
            let (specs, values) = Self::parse_fields(item.get("fields"))?;
            let sparse = Self::parse_sparse(item.get("sparse"))?;
            prepared.push(PreparedItem {
                ref_id,
                vector,
                specs,
                values,
                sparse,
            });
        }
        let dim = dim.expect("items niepuste → dim ustawione");
        let sparse_ns = prepared.iter().any(|p| p.sparse.is_some());

        // Faza 2 — zapis. Walidacja przeszła, więc backend dostaje tylko
        // poprawne itemy. Pierwszy item wytwarza/otwiera namespace ze schematem.
        let mut written = 0u64;
        for p in &prepared {
            let count = ctx
                .vectors
                .upsert_with_quota(
                    &org,
                    addon,
                    &namespace,
                    p.ref_id,
                    &p.vector,
                    dim,
                    metric,
                    &p.specs,
                    &p.values,
                    sparse_ns,
                    p.sparse.as_ref(),
                )
                .map_err(|e| anyhow!("vector adapter: upsert: {e}"))?;
            written += 1;
            out.meta
                .insert("vector_count".into(), serde_json::json!(count));
        }

        out.payload = FlowValue::Json(serde_json::json!({
            "op": "upsert",
            "namespace": namespace,
            "written": written,
        }));
        Ok(())
    }

    /// Op search: dense k-NN. Query z payload (Embedding/Json), filter+
    /// output_fields z node.config.
    fn op_search(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let namespace = Self::pick_namespace(node)?;
        let top_k = Self::parse_top_k(node)?;
        let query = Self::query_vector(envelope)?;
        let filter = parse_filter(node.config.get("filter"))?;
        let output_fields = parse_output_fields(node.config.get("output_fields"));

        let backend = ctx
            .vectors
            .get(&org, addon, &namespace)
            .map_err(|e| anyhow!("vector adapter: search: {e}"))?;
        let hits = backend
            .search(&query, top_k, filter.as_ref(), &output_fields)
            .map_err(|e| anyhow!("vector adapter: search: {e}"))?;

        out.payload = FlowValue::Json(hits_to_json(&namespace, "search", hits));
        Ok(())
    }

    /// Op hybrid: dense + sparse fuzja. Query Json{vector,sparse}; fusion z config.
    fn op_hybrid(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let namespace = Self::pick_namespace(node)?;
        let top_k = Self::parse_top_k(node)?;
        let query = Self::query_vector(envelope)?;

        let sparse_json = match &envelope.payload {
            FlowValue::Json(obj) => obj.get("sparse"),
            _ => None,
        };
        let sparse = Self::parse_sparse(sparse_json)?.ok_or_else(|| {
            anyhow!("vector adapter: hybrid: payload Json wymaga 'sparse' {{indices,values}}")
        })?;
        let fusion = Self::parse_fusion(node)?;
        let filter = parse_filter(node.config.get("filter"))?;
        let output_fields = parse_output_fields(node.config.get("output_fields"));

        let backend = ctx
            .vectors
            .get(&org, addon, &namespace)
            .map_err(|e| anyhow!("vector adapter: hybrid: {e}"))?;
        let hits = backend
            .hybrid_search(
                &query,
                &sparse,
                top_k,
                filter.as_ref(),
                &output_fields,
                fusion,
            )
            .map_err(|e| anyhow!("vector adapter: hybrid: {e}"))?;

        out.payload = FlowValue::Json(hits_to_json(&namespace, "hybrid", hits));
        Ok(())
    }
}

impl Default for VectorNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for VectorNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("vector adapter: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;
        let mut out: FlowEnvelope = (**envelope).clone();

        match Self::pick_op(node)? {
            "upsert" => Self::op_upsert(node, envelope, ctx, &mut out).await?,
            "search" => Self::op_search(node, envelope, ctx, &mut out)?,
            "hybrid" => Self::op_hybrid(node, envelope, ctx, &mut out)?,
            other => {
                return Err(anyhow!(
                    "vector adapter: nieznane 'op'='{other}' (upsert|search|hybrid)"
                ))
            }
        }
        Ok(out)
    }
}

/// Konwersja JSON-owej tablicy na `Vec<f32>` z błędem przy nie-liczbie.
fn json_f32_array(arr: &[serde_json::Value]) -> Result<Vec<f32>> {
    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| anyhow!("vector adapter: element[{i}] wektora nie jest liczbą"))
        })
        .collect()
}

/// JSON value → (FieldType, FieldValue). Liczba całkowita w zakresie i64 →
/// Int; poza zakresem / ułamkowa → Float (bez panika dla >i64::MAX).
fn json_to_field_value(v: &serde_json::Value, i: usize) -> Result<(FieldType, FieldValue)> {
    if let Some(s) = v.as_str() {
        return Ok((FieldType::Str, FieldValue::Str(s.to_string())));
    }
    if let Some(b) = v.as_bool() {
        return Ok((FieldType::Bool, FieldValue::Bool(b)));
    }
    if let Some(n) = v.as_i64() {
        return Ok((FieldType::Int, FieldValue::Int(n)));
    }
    if let Some(f) = v.as_f64() {
        return Ok((FieldType::Float, FieldValue::Float(f)));
    }
    Err(anyhow!(
        "vector adapter: field[{i}].value musi być str|int|float|bool, dostał {v}"
    ))
}

/// Lista nazw pól metadanych do zwrócenia na każdym hicie.
fn parse_output_fields(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Parsuje filter AST z JSON (rekurencyjnie). `None` gdy brak. Wspiera
/// `{eq|ne|gt|gte|lt|lte:[field,value]}`, `{in:[field,[values]]}`,
/// `{and|or:[...]}`, `{not: filter}`.
fn parse_filter(v: Option<&serde_json::Value>) -> Result<Option<Filter>> {
    let Some(obj) = v else {
        return Ok(None);
    };
    if obj.is_null() {
        return Ok(None);
    }
    Ok(Some(parse_filter_inner(obj)?))
}

fn parse_filter_inner(v: &serde_json::Value) -> Result<Filter> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("vector adapter: filter musi być obiektem"))?;
    let (key, val) = obj
        .iter()
        .next()
        .ok_or_else(|| anyhow!("vector adapter: pusty obiekt filter"))?;

    let cmp_pair = || -> Result<(String, FieldValue)> {
        let arr = val
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(|| anyhow!("vector adapter: filter '{key}' wymaga [field, value]"))?;
        let field = arr[0]
            .as_str()
            .ok_or_else(|| anyhow!("vector adapter: filter '{key}': field nie jest stringiem"))?
            .to_string();
        let (_t, value) = json_to_field_value(&arr[1], 0)?;
        Ok((field, value))
    };

    match key.as_str() {
        "eq" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Eq(f, v))
        }
        "ne" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Ne(f, v))
        }
        "gt" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Gt(f, v))
        }
        "gte" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Gte(f, v))
        }
        "lt" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Lt(f, v))
        }
        "lte" => {
            let (f, v) = cmp_pair()?;
            Ok(Filter::Lte(f, v))
        }
        "in" => {
            let arr = val
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or_else(|| anyhow!("vector adapter: filter 'in' wymaga [field, [values]]"))?;
            let field = arr[0]
                .as_str()
                .ok_or_else(|| anyhow!("vector adapter: filter 'in': field nie jest stringiem"))?
                .to_string();
            let values_arr = arr[1]
                .as_array()
                .ok_or_else(|| anyhow!("vector adapter: filter 'in': drugi element musi być tablicą"))?;
            let mut values = Vec::with_capacity(values_arr.len());
            for item in values_arr {
                values.push(json_to_field_value(item, 0)?.1);
            }
            Ok(Filter::In(field, values))
        }
        "and" => Ok(Filter::And(parse_filter_list(val)?)),
        "or" => Ok(Filter::Or(parse_filter_list(val)?)),
        "not" => Ok(Filter::Not(Box::new(parse_filter_inner(val)?))),
        other => Err(anyhow!("vector adapter: nieznany operator filter '{other}'")),
    }
}

fn parse_filter_list(v: &serde_json::Value) -> Result<Vec<Filter>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("vector adapter: and/or wymaga tablicy filtrów"))?;
    arr.iter().map(parse_filter_inner).collect()
}

/// Serializuje hity do `Json{op, namespace, hits:[{ref_id, score, fields}]}`,
/// posortowane rosnąco po score (najbliższe pierwsze — backend gwarantuje, ale
/// utrwalamy kontrakt w wyjściu).
fn hits_to_json(
    namespace: &str,
    op: &str,
    hits: Vec<crate::services::vector::backend::SearchHit>,
) -> serde_json::Value {
    let hits_json: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|h| {
            let fields: serde_json::Map<String, serde_json::Value> = h
                .fields
                .into_iter()
                .map(|f| (f.name, field_value_to_json(f.value)))
                .collect();
            serde_json::json!({
                "ref_id": h.ref_id,
                "score": h.score,
                "fields": fields,
            })
        })
        .collect();
    serde_json::json!({
        "op": op,
        "namespace": namespace,
        "hits": hits_json,
    })
}

fn field_value_to_json(v: FieldValue) -> serde_json::Value {
    match v {
        FieldValue::Str(s) => serde_json::Value::String(s),
        FieldValue::Int(n) => serde_json::json!(n),
        FieldValue::Float(f) => serde_json::json!(f),
        FieldValue::Bool(b) => serde_json::Value::Bool(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, stub_vectors};
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "v1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    /// Kontekst addona — `addon_id`/`org_id` Some, wspólny `vectors` żeby
    /// upsert i search trafiały w tę samą przestrzeń.
    fn addon_ctx(
        addon: &str,
        org: &str,
        vectors: Arc<crate::services::vector::NamespaceManager>,
    ) -> ExecutionContext {
        let mut ctx = stub_ctx();
        ctx.addon_id = Some(addon.to_string());
        ctx.org_id = Some(org.to_string());
        ctx.vectors = vectors;
        ctx
    }

    fn upsert_payload(items: serde_json::Value) -> FlowValue {
        FlowValue::Json(json!({ "items": items }))
    }

    #[tokio::test]
    async fn upsert_then_count_reported() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let out = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "passages", "metric": "cosine"})),
                &[input(upsert_payload(json!([
                    {"ref_id": 1, "vector": [1.0, 0.0, 0.0, 0.0]},
                    {"ref_id": 2, "vector": [0.0, 1.0, 0.0, 0.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap();
        let written = match &out.payload {
            FlowValue::Json(v) => v.get("written").and_then(|n| n.as_u64()).unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(written, 2);
        assert_eq!(out.meta.get("vector_count").and_then(|n| n.as_u64()), Some(2));
    }

    #[tokio::test]
    async fn search_returns_sorted_hits() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 4})),
                &[input(upsert_payload(json!([
                    {"ref_id": 10, "vector": [1.0, 0.0, 0.0, 0.0]},
                    {"ref_id": 20, "vector": [0.0, 1.0, 0.0, 0.0]},
                    {"ref_id": 30, "vector": [0.9, 0.1, 0.0, 0.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap();

        let out = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "search", "namespace": "p", "top_k": 2})),
                &[input(FlowValue::Embedding(vec![1.0, 0.0, 0.0, 0.0]))],
                &ctx,
            )
            .await
            .unwrap();

        let hits = match &out.payload {
            FlowValue::Json(v) => v.get("hits").and_then(|h| h.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(hits.len(), 2);
        // Najbliższy [1,0,0,0] to ref 10; sortowanie rosnąco po dystansie.
        assert_eq!(hits[0]["ref_id"].as_u64(), Some(10));
        let s0 = hits[0]["score"].as_f64().unwrap();
        let s1 = hits[1]["score"].as_f64().unwrap();
        assert!(s0 <= s1, "score rosnąco: {s0} <= {s1}");
    }

    #[tokio::test]
    async fn isolation_per_org_addon_namespace() {
        // Ten sam manager, trzy konteksty. Izolacja jest po kluczu
        // `(org, addon_instance, namespace)`. PK tabeli to `(addon_id,
        // namespace)`, więc produkcyjny inwariant: różne org → różne instance_id
        // (per-org install path nadaje unikalny instance_id). Test to lustrzy:
        // inst-a-org1 / inst-b-org1 (izolacja per-addon w org) oraz
        // inst-a-org2 (inna org = inny instance_id).
        let v = stub_vectors();
        let ctx_a = addon_ctx("inst-a-org1", "org-1", v.clone());
        let ctx_b = addon_ctx("inst-b-org1", "org-1", v.clone());
        let ctx_c = addon_ctx("inst-a-org2", "org-2", v.clone());

        for ctx in [&ctx_a, &ctx_b, &ctx_c] {
            VectorNodeAdapter::new()
                .execute(
                    &node(json!({"op": "upsert", "namespace": "ns", "dim": 3})),
                    &[input(upsert_payload(json!([
                        {"ref_id": 1, "vector": [1.0, 0.0, 0.0]},
                    ])))],
                    ctx,
                )
                .await
                .unwrap();
        }

        // inst-a/org-1: dorzuć drugi wektor, search ma zwrócić oba; inst-b i
        // inst-a/org-2 dalej mają po jednym.
        VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "ns", "dim": 3})),
                &[input(upsert_payload(json!([
                    {"ref_id": 2, "vector": [0.0, 1.0, 0.0]},
                ])))],
                &ctx_a,
            )
            .await
            .unwrap();

        let count = |out: &FlowEnvelope| match &out.payload {
            FlowValue::Json(v) => v.get("hits").and_then(|h| h.as_array()).map(|a| a.len()).unwrap(),
            _ => panic!("expected Json"),
        };
        let search_node = node(json!({"op": "search", "namespace": "ns", "top_k": 10}));
        let q = || input(FlowValue::Embedding(vec![1.0, 0.0, 0.0]));

        let oa = VectorNodeAdapter::new().execute(&search_node, &[q()], &ctx_a).await.unwrap();
        let ob = VectorNodeAdapter::new().execute(&search_node, &[q()], &ctx_b).await.unwrap();
        let oc = VectorNodeAdapter::new().execute(&search_node, &[q()], &ctx_c).await.unwrap();
        assert_eq!(count(&oa), 2, "inst-a/org-1 ma 2 wektory");
        assert_eq!(count(&ob), 1, "inst-b/org-1 ma 1 wektor (izolacja per-addon)");
        assert_eq!(count(&oc), 1, "inst-a/org-2 ma 1 wektor (izolacja per-org)");
    }

    #[tokio::test]
    async fn missing_addon_id_is_error_not_write() {
        let v = stub_vectors();
        let mut ctx = stub_ctx();
        ctx.vectors = v;
        // addon_id None (wywołanie nie-addonowe).
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 3})),
                &[input(upsert_payload(json!([
                    {"ref_id": 1, "vector": [1.0, 0.0, 0.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("brak tożsamości addona"),
            "błąd ma wskazywać brak addon_id, był: {err}"
        );
    }

    #[tokio::test]
    async fn ref_id_above_i64_max_does_not_panic() {
        // bug 1: u64 > i64::MAX nie panikuje — przechodzi (legalny u64 ref_id).
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let big = u64::MAX; // > i64::MAX
        let out = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([
                    {"ref_id": big, "vector": [1.0, 0.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap();
        let written = match &out.payload {
            FlowValue::Json(v) => v.get("written").and_then(|n| n.as_u64()).unwrap(),
            _ => panic!("expected Json"),
        };
        assert_eq!(written, 1);
    }

    #[tokio::test]
    async fn negative_ref_id_is_error_not_panic() {
        // bug 1: wartość spoza u64 (ujemna) → błąd, nie panic.
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([
                    {"ref_id": -5, "vector": [1.0, 0.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ref_id"), "był: {err}");
    }

    #[tokio::test]
    async fn top_k_above_u32_is_rejected_not_wrapped() {
        // bug 3: 4294967297 (= u32::MAX+2) NIE zawija do 1 — odrzucone.
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([{"ref_id": 1, "vector": [1.0, 0.0]}])))],
                &ctx,
            )
            .await
            .unwrap();
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "search", "namespace": "p", "top_k": 4294967297u64})),
                &[input(FlowValue::Embedding(vec![1.0, 0.0]))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("top_k") && err.to_string().contains("u32"),
            "top_k poza u32 ma być odrzucone z błędem zakresu, był: {err}"
        );
    }

    #[tokio::test]
    async fn top_k_above_max_is_rejected() {
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([{"ref_id": 1, "vector": [1.0, 0.0]}])))],
                &ctx,
            )
            .await
            .unwrap();
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "search", "namespace": "p", "top_k": 5000})),
                &[input(FlowValue::Embedding(vec![1.0, 0.0]))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("poza zakresem"), "był: {err}");
    }

    #[tokio::test]
    async fn bad_ref_id_in_batch_writes_nothing() {
        // bug 4: walidacja-przed-zapisem — drugi item ma ujemny ref_id, więc
        // CAŁY batch jest odrzucony zanim cokolwiek trafi do backendu.
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([
                    {"ref_id": 1, "vector": [1.0, 0.0]},
                    {"ref_id": -1, "vector": [0.0, 1.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ref_id"), "był: {err}");

        // Namespace nie powstał / nie ma żadnego wektora — search musi paść
        // NamespaceNotFound (pierwszy item TEŻ nie został zapisany).
        let search_err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "search", "namespace": "p", "top_k": 5})),
                &[input(FlowValue::Embedding(vec![1.0, 0.0]))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            search_err.to_string().to_lowercase().contains("not")
                || search_err.to_string().contains("nie"),
            "namespace nie powinien istnieć po odrzuconym batchu, był: {search_err}"
        );
    }

    #[tokio::test]
    async fn ref_id_zero_in_batch_writes_nothing() {
        // bug E1.0: ref_id=0 odrzucany jest przez backend zvec dopiero przy
        // zapisie (faza 2). Bez walidacji w fazie 1 pierwszy (poprawny) item
        // zostałby zapisany ZANIM drugi (ref_id=0) zwróci błąd → częściowy
        // zapis. Tu drugi item ma ref_id=0, więc CAŁY batch pada przed zapisem.
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(upsert_payload(json!([
                    {"ref_id": 1, "vector": [1.0, 0.0]},
                    {"ref_id": 0, "vector": [0.0, 1.0]},
                ])))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ref_id"), "był: {err}");

        // Pierwszy item NIE został zapisany — namespace nie istnieje, search pada.
        let search_err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "search", "namespace": "p", "top_k": 5})),
                &[input(FlowValue::Embedding(vec![1.0, 0.0]))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            search_err.to_string().to_lowercase().contains("not")
                || search_err.to_string().contains("nie"),
            "namespace nie powinien istnieć po odrzuconym batchu, był: {search_err}"
        );
    }

    #[tokio::test]
    async fn sparse_index_above_u32_is_error() {
        // bug 2: sparse index > u32::MAX → błąd, nie zawinięcie.
        let v = stub_vectors();
        let ctx = addon_ctx("inst-a", "org-1", v);
        let err = VectorNodeAdapter::new()
            .execute(
                &node(json!({"op": "upsert", "namespace": "p", "dim": 2})),
                &[input(FlowValue::Json(json!({"items": [
                    {"ref_id": 1, "vector": [1.0, 0.0], "sparse": {
                        "indices": [4294967296u64], "values": [0.5]
                    }},
                ]})))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("sparse.indices") && err.to_string().contains("u32"),
            "sparse index poza u32 ma być błędem, był: {err}"
        );
    }
}

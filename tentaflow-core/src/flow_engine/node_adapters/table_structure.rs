// =============================================================================
// Plik: flow_engine/node_adapters/table_structure.rs
// Opis: TableStructureNodeAdapter — rekonstrukcja struktury tabeli z obrazu
//       (region tabeli) na markdown GFM przez typed surface Documents (`/v1/infer`,
//       task=table_structure). Z DocRegion.cells (bbox+text) składa siatkę
//       wiersz/kolumna i renderuje tabelę GFM. Input: image(Image) → output:
//       table(Text).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use tentaflow_protocol::{DocCell, DocRegion};

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::page_detect::resolve_image;
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "table_structure";
const DEFAULT_MODEL: &str = "rag-table-structure";
const TASK: &str = "table_structure";
/// Tolerancja (w pikselach) grupowania komórek w wiersze/kolumny: komórki,
/// których środek y (wiersze) lub x (kolumny) różni się o mniej niż próg,
/// traktujemy jako tę samą linię siatki. Detektory nie zwracają idealnie
/// wyrównanych bboxów, więc bez tolerancji każda komórka byłaby osobnym wierszem.
const GRID_TOLERANCE: f32 = 8.0;

pub struct TableStructureNodeAdapter;

impl TableStructureNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> String {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        if let Some(m) = envelope
            .meta
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        DEFAULT_MODEL.to_string()
    }

    /// Składa markdown GFM z komórek pierwszego regionu, który je niesie.
    /// Pusto / brak komórek → pusty string (caller dostaje pustą tabelę, nie
    /// błąd — region może nie mieć rozpoznanej struktury).
    fn cells_to_markdown(regions: &[DocRegion]) -> String {
        let Some(cells) = regions.iter().find_map(|r| r.cells.as_ref()) else {
            return String::new();
        };
        if cells.is_empty() {
            return String::new();
        }
        Self::grid_to_markdown(cells)
    }

    /// Grupuje komórki w wiersze po środku y, kolumny po środku x (z tolerancją),
    /// a następnie renderuje siatkę jako tabelę GFM. Brakujące komórki w siatce
    /// to puste pola. Pierwszy wiersz traktujemy jako nagłówek (GFM wymaga
    /// separatora `---` po pierwszym wierszu).
    fn grid_to_markdown(cells: &[DocCell]) -> String {
        // Środki kolumn i wierszy — sortujemy i scalamy bliskie linie.
        let mut row_centers = Self::cluster_centers(cells.iter().map(|c| Self::cy(c.bbox)));
        let mut col_centers = Self::cluster_centers(cells.iter().map(|c| Self::cx(c.bbox)));
        row_centers.sort_by(|a, b| a.total_cmp(b));
        col_centers.sort_by(|a, b| a.total_cmp(b));
        if row_centers.is_empty() || col_centers.is_empty() {
            return String::new();
        }

        let cols = col_centers.len();
        let mut grid: Vec<Vec<String>> = vec![vec![String::new(); cols]; row_centers.len()];
        for c in cells {
            let r = Self::nearest_index(&row_centers, Self::cy(c.bbox));
            let col = Self::nearest_index(&col_centers, Self::cx(c.bbox));
            let text = c.text.trim().replace('|', "\\|").replace('\n', " ");
            // Jeśli dwie komórki trafiają w to samo pole (zlepione bboxy),
            // doklejamy spacją zamiast nadpisywać.
            if grid[r][col].is_empty() {
                grid[r][col] = text;
            } else if !text.is_empty() {
                grid[r][col].push(' ');
                grid[r][col].push_str(&text);
            }
        }

        let mut out = String::new();
        for (i, row) in grid.iter().enumerate() {
            out.push_str("| ");
            out.push_str(&row.join(" | "));
            out.push_str(" |\n");
            if i == 0 {
                out.push('|');
                for _ in 0..cols {
                    out.push_str(" --- |");
                }
                out.push('\n');
            }
        }
        out
    }

    fn cx(b: [f32; 4]) -> f32 {
        (b[0] + b[2]) / 2.0
    }
    fn cy(b: [f32; 4]) -> f32 {
        (b[1] + b[3]) / 2.0
    }

    /// Scala bliskie współrzędne (różnica < GRID_TOLERANCE) w jeden środek linii.
    fn cluster_centers(values: impl Iterator<Item = f32>) -> Vec<f32> {
        let mut sorted: Vec<f32> = values.collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let mut centers: Vec<f32> = Vec::new();
        for v in sorted {
            match centers.last() {
                Some(&last) if (v - last).abs() < GRID_TOLERANCE => {}
                _ => centers.push(v),
            }
        }
        centers
    }

    fn nearest_index(centers: &[f32], v: f32) -> usize {
        centers
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (*a - v).abs().total_cmp(&(*b - v).abs()))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

impl Default for TableStructureNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for TableStructureNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("table", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("{NODE_TYPE}: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = resolve_image(envelope)?;
        let image = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: pobranie obrazu: {e}"))?;
        if image.is_empty() {
            return Err(anyhow!("{NODE_TYPE}: pusty obraz tabeli"));
        }
        let model = Self::pick_model(node, envelope);

        let result = ctx
            .documents
            .infer(&model, &image, &mime, TASK, ctx.provenance())
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: detektor zawiódł: {e}"))?;

        let markdown = Self::cells_to_markdown(&result.regions);

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(markdown);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "ts1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn cell(x1: f32, y1: f32, x2: f32, y2: f32, text: &str) -> DocCell {
        DocCell {
            bbox: [x1, y1, x2, y2],
            text: text.into(),
        }
    }

    /// 2x2 tabela: bboxy komórek w dwóch wierszach i dwóch kolumnach → GFM z
    /// separatorem nagłówka. Próba, że reading-order po bbox działa nawet gdy
    /// komórki przychodzą w przypadkowej kolejności.
    #[test]
    fn cells_render_gfm_table_in_grid_order() {
        let region = DocRegion {
            class: "table".into(),
            bbox: [0.0, 0.0, 100.0, 100.0],
            score: 0.9,
            cells: Some(vec![
                cell(60.0, 0.0, 100.0, 20.0, "B1"),
                cell(0.0, 0.0, 50.0, 20.0, "A1"),
                cell(0.0, 40.0, 50.0, 60.0, "A2"),
                cell(60.0, 40.0, 100.0, 60.0, "B2"),
            ]),
            ocr_spans: None,
        };
        let md = TableStructureNodeAdapter::cells_to_markdown(&[region]);
        let expected = "| A1 | B1 |\n| --- | --- |\n| A2 | B2 |\n";
        assert_eq!(md, expected);
    }

    #[test]
    fn empty_cells_yield_empty_markdown() {
        let region = DocRegion {
            class: "table".into(),
            bbox: [0.0; 4],
            score: 0.5,
            cells: Some(vec![]),
            ocr_spans: None,
        };
        assert_eq!(TableStructureNodeAdapter::cells_to_markdown(&[region]), "");
        // Brak cells w ogóle → też pusto.
        let region2 = DocRegion {
            class: "table".into(),
            bbox: [0.0; 4],
            score: 0.5,
            cells: None,
            ocr_spans: None,
        };
        assert_eq!(TableStructureNodeAdapter::cells_to_markdown(&[region2]), "");
    }

    #[test]
    fn pipe_in_cell_text_is_escaped() {
        let region = DocRegion {
            class: "table".into(),
            bbox: [0.0; 4],
            score: 0.5,
            cells: Some(vec![cell(0.0, 0.0, 10.0, 10.0, "a|b")]),
            ocr_spans: None,
        };
        let md = TableStructureNodeAdapter::cells_to_markdown(&[region]);
        assert!(md.contains("a\\|b"), "pipe musi być eskejpowany: {md}");
    }

    #[tokio::test]
    async fn stub_documents_yields_empty_table_text() {
        let ctx = stub_ctx();
        let blob = ctx.blobs.put(vec![1u8; 16], "image/png").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: blob,
            mime: "image/png".into(),
            dims: None,
        };
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "images".into(),
            envelope: Arc::new(env),
        };
        let out = TableStructureNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        assert!(matches!(out.payload, FlowValue::Text(t) if t.is_empty()));
    }
}

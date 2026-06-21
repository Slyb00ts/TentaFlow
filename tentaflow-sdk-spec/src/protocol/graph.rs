// =============================================================================
// File: protocol/graph.rs — graph storage host-function ABI payloads (RAG 0.2)
// Purpose: single source of truth for the CBOR request/response structs of the
// seven `graph_*_v1` host functions backing the embedded CozoDB graph engine
// (services/graph). Shared verbatim by the core host (decode input / encode
// output) and the addon SDK (encode input / decode output) so the wire format
// cannot drift. Node/edge property bags reuse the universal `FieldValue` type;
// maps use integer keys via `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

use super::vector_query::FieldValue;

// -----------------------------------------------------------------------------
// Shared shapes
// -----------------------------------------------------------------------------

/// Provenance of an extracted node/edge — links the graph fact back to its
/// source chunk/document so Etap 1 citation can resolve a node to a page span.
/// Every field is optional on the wire: a hand-authored fact may carry only a
/// `doc_id`, a high-confidence extractor may fill the whole struct.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Provenance {
    /// Source chunk id (retrieval unit the fact was extracted from).
    #[n(0)]
    pub chunk_id: Option<String>,
    /// Source document id.
    #[n(1)]
    pub doc_id: Option<String>,
    /// 1-based page number inside the source document.
    #[n(2)]
    pub page: Option<u32>,
    /// Character span `[start, end)` inside the chunk/page text.
    #[n(3)]
    pub span: Option<(u32, u32)>,
    /// Extractor confidence in `[0, 1]`.
    #[n(4)]
    pub confidence: Option<f32>,
    /// Version string of the extractor that produced this fact.
    #[n(5)]
    pub extractor_version: Option<String>,
}

/// One named property on a node/edge. Mirrors `vector_query::Field` but kept as
/// a dedicated map shape so the graph and vector schemas stay independent.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphProp {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub value: FieldValue,
}

/// A graph node: stable string id, a label (node type), a typed property bag and
/// optional provenance.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphNode {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub label: String,
    #[n(2)]
    pub props: Vec<GraphProp>,
    #[n(3)]
    pub provenance: Option<Provenance>,
}

// -----------------------------------------------------------------------------
// graph_upsert_node_v1
// -----------------------------------------------------------------------------

/// Input for `graph_upsert_node_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphUpsertNodeInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub node: GraphNode,
}

/// Output of `graph_upsert_node_v1`. `count` is the post-upsert node count.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphUpsertNodeOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub id: String,
    #[n(2)]
    pub count: u64,
}

// -----------------------------------------------------------------------------
// graph_upsert_edge_v1
// -----------------------------------------------------------------------------

/// Input for `graph_upsert_edge_v1` — a directed `src -[rel]-> dst` edge with a
/// weight (default 1.0 when absent), a property bag and optional provenance.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphUpsertEdgeInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub src: String,
    #[n(2)]
    pub rel: String,
    #[n(3)]
    pub dst: String,
    #[n(4)]
    pub weight: Option<f64>,
    #[n(5)]
    pub props: Vec<GraphProp>,
    #[n(6)]
    pub provenance: Option<Provenance>,
}

/// Output of `graph_upsert_edge_v1`. `count` is the post-upsert edge count.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphUpsertEdgeOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub count: u64,
}

// -----------------------------------------------------------------------------
// graph_neighbors_v1
// -----------------------------------------------------------------------------

/// Edge traversal direction for `graph_neighbors_v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum GraphDirection {
    /// Follow out-edges (`src == node`).
    #[n(0)]
    Out,
    /// Follow in-edges (`dst == node`).
    #[n(1)]
    In,
    /// Follow both directions.
    #[n(2)]
    Both,
}

/// Input for `graph_neighbors_v1`. `rel` optionally filters by relation type.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphNeighborsInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub node: String,
    #[n(2)]
    pub direction: GraphDirection,
    #[n(3)]
    pub rel: Option<String>,
    #[n(4)]
    pub limit: u32,
}

/// One neighbor reached from the seed node.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphNeighbor {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub rel: String,
    #[n(2)]
    pub weight: f64,
}

/// Output of `graph_neighbors_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphNeighborsOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub neighbors: Vec<GraphNeighbor>,
}

// -----------------------------------------------------------------------------
// graph_pagerank_v1
// -----------------------------------------------------------------------------

/// Input for `graph_pagerank_v1` (built-in Cozo PageRank). `top_n` caps the
/// returned ranking; `damping`/`iterations` are optional tuning knobs.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphPagerankInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub top_n: u32,
    #[n(2)]
    pub damping: Option<f64>,
    #[n(3)]
    pub iterations: Option<u32>,
}

/// One ranked node (PageRank or PPR).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphRankedNode {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub score: f64,
}

/// Output of `graph_pagerank_v1` — top-N nodes, highest score first.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphPagerankOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub ranked: Vec<GraphRankedNode>,
}

// -----------------------------------------------------------------------------
// graph_ppr_v1 (Personalized PageRank computed in Rust over CSR)
// -----------------------------------------------------------------------------

/// One personalization seed for `graph_ppr_v1`. `weight` lets the addon bias
/// the teleportation mass; absent = uniform across seeds.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphSeed {
    #[n(0)]
    pub id: String,
    #[n(1)]
    pub weight: Option<f64>,
}

/// Input for `graph_ppr_v1`. `seeds` is the personalization vector.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphPprInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub seeds: Vec<GraphSeed>,
    #[n(2)]
    pub top_n: u32,
    #[n(3)]
    pub damping: Option<f64>,
    #[n(4)]
    pub iterations: Option<u32>,
}

/// Output of `graph_ppr_v1` — top-N nodes by personalized score.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphPprOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub ranked: Vec<GraphRankedNode>,
}

// -----------------------------------------------------------------------------
// graph_delete_v1
// -----------------------------------------------------------------------------

/// What `graph_delete_v1` removes.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub enum GraphDeleteTarget {
    /// Delete a node by id (and its incident edges).
    #[n(0)]
    Node(#[n(0)] String),
    /// Delete a single directed edge `(src, rel, dst)`.
    #[n(1)]
    Edge(#[n(0)] String, #[n(1)] String, #[n(2)] String),
    /// Soft-delete a node: keep the row but mark it tombstoned (`label` set to
    /// the tombstone marker, props/provenance cleared). Edges are left intact so
    /// provenance chains survive; retrieval filters tombstoned nodes out.
    #[n(2)]
    Tombstone(#[n(0)] String),
}

/// Input for `graph_delete_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphDeleteInput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub target: GraphDeleteTarget,
}

/// Output of `graph_delete_v1`. `removed` is true when the target existed.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct GraphDeleteOutput {
    #[n(0)]
    pub collection: String,
    #[n(1)]
    pub removed: bool,
    #[n(2)]
    pub node_count: u64,
    #[n(3)]
    pub edge_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_node_io() {
        roundtrip(&GraphUpsertNodeInput {
            collection: "kg".into(),
            node: GraphNode {
                id: "n1".into(),
                label: "Person".into(),
                props: vec![GraphProp {
                    name: "name".into(),
                    value: FieldValue::Str("Ada".into()),
                }],
                provenance: Some(Provenance {
                    chunk_id: Some("c1".into()),
                    doc_id: Some("d1".into()),
                    page: Some(3),
                    span: Some((10, 42)),
                    confidence: Some(0.91),
                    extractor_version: Some("v1".into()),
                }),
            },
        });
        roundtrip(&GraphUpsertNodeOutput {
            collection: "kg".into(),
            id: "n1".into(),
            count: 5,
        });
    }

    #[test]
    fn roundtrip_edge_io() {
        roundtrip(&GraphUpsertEdgeInput {
            collection: "kg".into(),
            src: "n1".into(),
            rel: "KNOWS".into(),
            dst: "n2".into(),
            weight: Some(0.7),
            props: vec![],
            provenance: None,
        });
        roundtrip(&GraphUpsertEdgeOutput {
            collection: "kg".into(),
            count: 9,
        });
    }

    #[test]
    fn roundtrip_neighbors_io() {
        roundtrip(&GraphNeighborsInput {
            collection: "kg".into(),
            node: "n1".into(),
            direction: GraphDirection::Both,
            rel: Some("KNOWS".into()),
            limit: 50,
        });
        roundtrip(&GraphNeighborsOutput {
            collection: "kg".into(),
            neighbors: vec![GraphNeighbor {
                id: "n2".into(),
                rel: "KNOWS".into(),
                weight: 1.0,
            }],
        });
    }

    #[test]
    fn roundtrip_rank_io() {
        roundtrip(&GraphPagerankInput {
            collection: "kg".into(),
            top_n: 10,
            damping: Some(0.85),
            iterations: Some(20),
        });
        roundtrip(&GraphPprInput {
            collection: "kg".into(),
            seeds: vec![GraphSeed {
                id: "n1".into(),
                weight: Some(2.0),
            }],
            top_n: 10,
            damping: None,
            iterations: None,
        });
        roundtrip(&GraphPagerankOutput {
            collection: "kg".into(),
            ranked: vec![GraphRankedNode {
                id: "n1".into(),
                score: 0.42,
            }],
        });
    }

    #[test]
    fn roundtrip_delete_io() {
        roundtrip(&GraphDeleteInput {
            collection: "kg".into(),
            target: GraphDeleteTarget::Edge("a".into(), "R".into(), "b".into()),
        });
        roundtrip(&GraphDeleteInput {
            collection: "kg".into(),
            target: GraphDeleteTarget::Tombstone("n1".into()),
        });
        roundtrip(&GraphDeleteOutput {
            collection: "kg".into(),
            removed: true,
            node_count: 4,
            edge_count: 2,
        });
    }
}

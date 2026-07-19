// =============================================================================
// File: RerankEngine.swift
// Purpose: Native embedded jina-reranker-v3 reranker in Swift MLX. Replaces the
//          Python bundle. Reuses the Qwen3 decoder-only model loaded through
//          EmbedderModelFactory (same path as the embedder) plus a small MLP
//          projector loaded from `projector.safetensors`, then scores each
//          document against the query by cosine similarity of the projected
//          special-token hidden states. Scores are returned in INPUT order;
//          sorting is done by the Rust/core layer, not here.
// =============================================================================

import Foundation
import MLX
import MLXEmbedders
import MLXNN

/// Errors surfaced by the reranker compute/loading path.
enum RerankError: Error {
    case missingProjectorWeights(String)
    case queryTokenNotFound
    case documentTokenCountMismatch(expected: Int, found: Int)
    case emptyHiddenStates
}

/// Stateless helpers implementing the jina-reranker-v3 algorithm. The Qwen3
/// model itself is owned by `MLXBridgeEngine.embedderContainer`; the projector
/// weights are cached on the engine and passed in here.
enum RerankEngine {
    // Special tokens from the jina-reranker-v3 tokenizer. We locate their
    // positions by ID rather than by decoding, matching the reference
    // `input_ids == 151670` logic — the tokenizer may not expose these as
    // named/encodable strings, but it does emit the IDs when the raw marker
    // strings are present in the prompt.
    static let queryEmbedTokenId = 151671  // <|rerank_token|>
    static let docEmbedTokenId = 151670    // <|embed_token|>
    static let queryEmbedTokenStr = "<|rerank_token|>"
    static let docEmbedTokenStr = "<|embed_token|>"

    private static let systemPrefix =
        "<|im_start|>system\nYou are a search relevance expert who can determine a ranking of the passages based on how relevant they are to the query. If the query is a question, how relevant a passage is depends on how well it answers the question. If not, try to analyze the intent of the query and assess how well each passage satisfies the intent. If an instruction is provided, you should follow the instruction when determining the ranking.<|im_end|>\n<|im_start|>user\n"

    private static let assistantSuffix =
        "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"

    /// Loads the projector MLP weights from `<modelDir>/projector.safetensors`.
    /// Returns `(linear1.weight [512,1024], linear2.weight [512,512])` in
    /// PyTorch layout (weight is [out, in], so a forward pass is `x @ weight.T`).
    static func loadProjector(modelDir: String) throws -> (MLXArray, MLXArray) {
        let url = URL(filePath: modelDir).appending(component: "projector.safetensors")
        let weights = try MLX.loadArrays(url: url)
        guard let linear1 = weights["linear1.weight"] else {
            throw RerankError.missingProjectorWeights("linear1.weight")
        }
        guard let linear2 = weights["linear2.weight"] else {
            throw RerankError.missingProjectorWeights("linear2.weight")
        }
        return (linear1, linear2)
    }

    /// Strips raw special-token marker strings from user-supplied text so a
    /// document/query cannot inject extra query/doc anchors into the prompt.
    private static func sanitize(_ text: String) -> String {
        text
            .replacingOccurrences(of: queryEmbedTokenStr, with: "")
            .replacingOccurrences(of: docEmbedTokenStr, with: "")
    }

    /// Builds the exact jina-reranker-v3 prompt for a query and N documents.
    static func buildPrompt(query: String, documents: [String]) -> String {
        let cleanQuery = sanitize(query)
        let passages = documents.enumerated()
            .map { index, doc in
                "<passage id=\"\(index)\">\n\(sanitize(doc))\(docEmbedTokenStr)\n</passage>"
            }
            .joined(separator: "\n")

        let body =
            "I will provide you with \(documents.count) passages, each indicated by a numerical identifier. Rank the passages based on their relevance to query: \(cleanQuery)\n"
            + passages
            + "\n<query>\n\(cleanQuery)\(queryEmbedTokenStr)\n</query>"

        return systemPrefix + body + assistantSuffix
    }

    /// Runs the full reranker pipeline against an already-loaded Qwen3 embedder
    /// context and projector weights. Returns one cosine-similarity score per
    /// document, in the SAME order as `documents`.
    static func computeScores(
        context: EmbedderModelContext,
        projector: (linear1: MLXArray, linear2: MLXArray),
        query: String,
        documents: [String]
    ) throws -> [Float] {
        guard !documents.isEmpty else { return [] }

        let prompt = buildPrompt(query: query, documents: documents)
        // Prompt already carries <|im_start|> etc. and the special markers, so
        // do not let the tokenizer prepend/append its own special tokens.
        let ids = context.tokenizer.encode(text: prompt, addSpecialTokens: false)

        let input = MLXArray(ids).reshaped([1, ids.count])
        let mask = MLXArray.ones([1, ids.count])
        let output = context.model(
            input, positionIds: nil, tokenTypeIds: nil, attentionMask: mask)
        guard let hiddenStates = output.hiddenStates else {
            throw RerankError.emptyHiddenStates
        }
        let hidden = hiddenStates[0]  // [seq, hidden]

        // Locate anchor positions by token ID (see queryEmbedTokenId note).
        guard let queryPos = ids.firstIndex(of: queryEmbedTokenId) else {
            throw RerankError.queryTokenNotFound
        }
        let docPositions = ids.enumerated()
            .filter { $0.element == docEmbedTokenId }
            .map { $0.offset }
        guard docPositions.count == documents.count else {
            throw RerankError.documentTokenCountMismatch(
                expected: documents.count, found: docPositions.count)
        }

        let queryHidden = hidden[queryPos]  // [hidden]
        let docHidden = stacked(docPositions.map { hidden[$0] }, axis: 0)  // [N, hidden]

        // Projector MLP: relu(x @ linear1.T) @ linear2.T (no bias).
        let queryEmb = project(queryHidden, projector)  // [512]
        let docEmb = project(docHidden, projector)       // [N, 512]

        // Cosine similarity per document (batched).
        let dots = docEmb.matmul(queryEmb)                       // [N]
        let docNorms = sqrt((docEmb * docEmb).sum(axis: 1))      // [N]
        let queryNorm = sqrt((queryEmb * queryEmb).sum())        // scalar
        let scores = dots / (docNorms * queryNorm)               // [N]
        scores.eval()
        return scores.asArray(Float.self)
    }

    /// Applies the two-layer projector. `weight` is [out, in], so we matmul by
    /// its transpose to compute `x @ weight.T`. Works for both a 1-D query
    /// vector and a 2-D [N, hidden] document batch.
    private static func project(
        _ x: MLXArray, _ projector: (linear1: MLXArray, linear2: MLXArray)
    ) -> MLXArray {
        let h = relu(x.matmul(projector.linear1.T))
        return h.matmul(projector.linear2.T)
    }
}

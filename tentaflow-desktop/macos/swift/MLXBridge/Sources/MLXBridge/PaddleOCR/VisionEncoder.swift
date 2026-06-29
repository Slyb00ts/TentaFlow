import Foundation
import MLX
import MLXNN
import MLXFast

// =============================================================================
// PaddleOCR-VL vision tower (SigLIP/NaViT + 2D RoPE + spatial merge), port 1:1
// z modeling_paddleocr_vl.py. Klucze wag: vision_model.{embeddings, encoder,
// post_layernorm, head}. Sciezka VLM uzywa per-patch (post_layernorm), head
// laduje sie tylko dla zgodnosci wag. Pojedynczy obraz => pelna uwaga (bez
// cu_seqlens/window).
// =============================================================================

/// rotate_half: [-x2, x1] na ostatnim wymiarze.
private func rotateHalf(_ x: MLXArray) -> MLXArray {
    let d = x.dim(-1) / 2
    let x1 = x[.ellipsis, 0 ..< d]
    let x2 = x[.ellipsis, d ..< (2 * d)]
    return concatenated([-x2, x1], axis: -1)
}

/// SigLIP rotary embedding — inv_freq nad dim, forward(seqlen) -> [seqlen, dim/2].
public class SigLIPRotaryEmbedding {
    let invFreq: MLXArray  // [dim/2]

    public init(dim: Int, theta: Float = 10000.0) {
        let idx = MLXArray(stride(from: 0, to: dim, by: 2).map { Float($0) }) / Float(dim)
        self.invFreq = 1.0 / pow(MLXArray(theta), idx)
    }

    /// Zwraca freqs [seqlen, dim/2] = outer(arange(seqlen), invFreq).
    public func callAsFunction(_ seqlen: Int) -> MLXArray {
        let seq = MLXArray((0 ..< seqlen).map { Float($0) })
        return outer(seq, invFreq)
    }
}

public class PaddleOCRMLP: Module {
    @ModuleInfo(key: "fc1") var fc1: Linear
    @ModuleInfo(key: "fc2") var fc2: Linear

    public init(config: PaddleOCRVLVisionConfig) {
        self._fc1.wrappedValue = Linear(config.hiddenSize, config.intermediateSize)
        self._fc2.wrappedValue = Linear(config.intermediateSize, config.hiddenSize)
    }

    public func callAsFunction(_ x: MLXArray) -> MLXArray {
        // hidden_act = gelu_pytorch_tanh
        fc2(geluApproximate(fc1(x)))
    }
}

public class PaddleOCRAttention: Module {
    let numHeads: Int
    let headDim: Int
    let scale: Float

    @ModuleInfo(key: "q_proj") var qProj: Linear
    @ModuleInfo(key: "k_proj") var kProj: Linear
    @ModuleInfo(key: "v_proj") var vProj: Linear
    @ModuleInfo(key: "out_proj") var outProj: Linear

    public init(config: PaddleOCRVLVisionConfig) {
        self.numHeads = config.numAttentionHeads
        self.headDim = config.hiddenSize / config.numAttentionHeads
        self.scale = pow(Float(headDim), -0.5)
        self._qProj.wrappedValue = Linear(config.hiddenSize, config.hiddenSize)
        self._kProj.wrappedValue = Linear(config.hiddenSize, config.hiddenSize)
        self._vProj.wrappedValue = Linear(config.hiddenSize, config.hiddenSize)
        self._outProj.wrappedValue = Linear(config.hiddenSize, config.hiddenSize)
        super.init()
    }

    /// hidden: [B, N, D]; ropeCos/Sin: [N, headDim] albo nil. Pelna uwaga.
    public func callAsFunction(_ hidden: MLXArray, ropeCos: MLXArray?, ropeSin: MLXArray?) -> MLXArray {
        let b = hidden.dim(0), n = hidden.dim(1)
        var q = qProj(hidden).reshaped(b, n, numHeads, headDim)
        var k = kProj(hidden).reshaped(b, n, numHeads, headDim)
        let v = vProj(hidden).reshaped(b, n, numHeads, headDim).transposed(0, 2, 1, 3)

        if let cos = ropeCos, let sin = ropeSin {
            // apply_rotary_pos_emb_vision: cos/sin [N, headDim] -> [1, N, 1, headDim]
            let c = cos.reshaped(1, n, 1, headDim)
            let s = sin.reshaped(1, n, 1, headDim)
            q = (q * c) + (rotateHalf(q) * s)
            k = (k * c) + (rotateHalf(k) * s)
        }
        q = q.transposed(0, 2, 1, 3)
        k = k.transposed(0, 2, 1, 3)

        let out = MLXFast.scaledDotProductAttention(queries: q, keys: k, values: v, scale: scale, mask: .none)
        let merged = out.transposed(0, 2, 1, 3).reshaped(b, n, -1)
        return outProj(merged)
    }
}

public class PaddleOCREncoderLayer: Module {
    @ModuleInfo(key: "layer_norm1") var layerNorm1: LayerNorm
    @ModuleInfo(key: "self_attn") var selfAttn: PaddleOCRAttention
    @ModuleInfo(key: "layer_norm2") var layerNorm2: LayerNorm
    @ModuleInfo(key: "mlp") var mlp: PaddleOCRMLP

    public init(config: PaddleOCRVLVisionConfig) {
        self._layerNorm1.wrappedValue = LayerNorm(dimensions: config.hiddenSize, eps: config.layerNormEps)
        self._selfAttn.wrappedValue = PaddleOCRAttention(config: config)
        self._layerNorm2.wrappedValue = LayerNorm(dimensions: config.hiddenSize, eps: config.layerNormEps)
        self._mlp.wrappedValue = PaddleOCRMLP(config: config)
    }

    public func callAsFunction(_ hidden: MLXArray, ropeCos: MLXArray?, ropeSin: MLXArray?) -> MLXArray {
        var h = hidden + selfAttn(layerNorm1(hidden), ropeCos: ropeCos, ropeSin: ropeSin)
        h = h + mlp(layerNorm2(h))
        return h
    }
}

public class PaddleOCREncoder: Module {
    @ModuleInfo(key: "layers") var layers: [PaddleOCREncoderLayer]
    let rotary: SigLIPRotaryEmbedding
    let headDim: Int

    public init(config: PaddleOCRVLVisionConfig) {
        self.headDim = config.hiddenSize / config.numAttentionHeads
        self._layers.wrappedValue = (0 ..< config.numHiddenLayers).map { _ in PaddleOCREncoderLayer(config: config) }
        // SigLIPRotaryEmbedding(head_dim // 2)
        self.rotary = SigLIPRotaryEmbedding(dim: headDim / 2)
        super.init()
    }

    /// Pojedynczy obraz: grid (gh, gw), N = gh*gw. Buduje 2D rope i przepuszcza
    /// przez warstwy (pelna uwaga).
    public func callAsFunction(_ hidden: MLXArray, gridH: Int, gridW: Int) -> MLXArray {
        let n = gridH * gridW
        // height/width position ids (t=1): hids = idx // w, wids = idx % w
        var hids = [Int32](); hids.reserveCapacity(n)
        var wids = [Int32](); wids.reserveCapacity(n)
        for i in 0 ..< n {
            hids.append(Int32(i / gridW))
            wids.append(Int32(i % gridW))
        }
        let maxGrid = max(gridH, gridW)
        let freqs = rotary(maxGrid)  // [maxGrid, headDim/4]  (dim = headDim/2 -> /2 freqs)
        // rope_emb = freqs[pids].flatten(1) -> [N, headDim/2]; repeat(1,2) -> [N, headDim]
        let hFreq = freqs[MLXArray(hids)]  // [N, headDim/4]
        let wFreq = freqs[MLXArray(wids)]  // [N, headDim/4]
        let stacked = concatenated([hFreq, wFreq], axis: -1)  // [N, headDim/2]
        let ropeFull = concatenated([stacked, stacked], axis: -1)  // [N, headDim]
        let cos = MLX.cos(ropeFull)
        let sin = MLX.sin(ropeFull)

        var h = hidden
        for layer in layers {
            h = layer(h, ropeCos: cos, ropeSin: sin)
        }
        return h
    }
}

public class PaddleOCRVisionEmbeddings: Module {
    @ModuleInfo(key: "patch_embedding") var patchEmbedding: Conv2d
    @ModuleInfo(key: "position_embedding") var positionEmbedding: Embedding
    @ModuleInfo(key: "packing_position_embedding") var packingPositionEmbedding: Embedding

    let patchSize: Int
    let hiddenSize: Int
    let numPositions: Int

    public init(config: PaddleOCRVLVisionConfig) {
        self.patchSize = config.patchSize
        self.hiddenSize = config.hiddenSize
        let perSide = config.imageSize / config.patchSize
        self.numPositions = perSide * perSide

        self._patchEmbedding.wrappedValue = Conv2d(
            inputChannels: config.numChannels,
            outputChannels: config.hiddenSize,
            kernelSize: IntOrPair(config.patchSize),
            stride: IntOrPair(config.patchSize)
        )
        self._positionEmbedding.wrappedValue = Embedding(embeddingCount: numPositions, dimensions: config.hiddenSize)
        self._packingPositionEmbedding.wrappedValue = Embedding(embeddingCount: 32768, dimensions: config.hiddenSize)
        super.init()
    }

    /// Bilinearna interpolacja pos-emb do (gh, gw). pos table [numPositions, D]
    /// -> [1, sqrt, sqrt, D] -> resize -> [1, gh*gw, D].
    private func interpolatePos(gridH: Int, gridW: Int) -> MLXArray {
        let side = Int(Double(numPositions).squareRoot().rounded())
        let table = positionEmbedding.weight.reshaped(1, side, side, hiddenSize)
        if side == gridH && side == gridW {
            return table.reshaped(1, gridH * gridW, hiddenSize)
        }
        let up = Upsample(
            scaleFactor: [Float(gridH) / Float(side), Float(gridW) / Float(side)],
            mode: .linear(alignCorners: false)
        )
        let resized = up(table)  // [1, gh, gw, D]
        return resized.reshaped(1, gridH * gridW, hiddenSize)
    }

    /// image: [1, H, W, C] (channels-last). Conv stride=patch daje [1, gh, gw, D]
    /// w kolejnosci row-major (h, w) — zgodnej z rope i spatial-merge projektora.
    /// Zwraca [1, gh*gw, D].
    public func callAsFunction(_ image: MLXArray, gridH: Int, gridW: Int) -> MLXArray {
        let conv = patchEmbedding(image)  // [1, gh, gw, D]
        var embeds = conv.reshaped(1, gridH * gridW, hiddenSize)
        let pos = interpolatePos(gridH: gridH, gridW: gridW)  // [1, N, D]
        embeds = embeds + pos
        return embeds
    }
}

public class NaViTVisionEncoder: Module {
    @ModuleInfo(key: "embeddings") var embeddings: PaddleOCRVisionEmbeddings
    @ModuleInfo(key: "encoder") var encoder: PaddleOCREncoder
    @ModuleInfo(key: "post_layernorm") var postLayerNorm: LayerNorm
    // head (MAP pooling) NIE jest modelowany — w sciezce VLM nieuzywany, a jego
    // wagi (head.*) sa pomijane w sanitizeWeights.

    public init(config: PaddleOCRVLVisionConfig) {
        self._embeddings.wrappedValue = PaddleOCRVisionEmbeddings(config: config)
        self._encoder.wrappedValue = PaddleOCREncoder(config: config)
        self._postLayerNorm.wrappedValue = LayerNorm(dimensions: config.hiddenSize, eps: config.layerNormEps)
        super.init()
    }

    /// Zwraca per-patch features [1, N, D] (sciezka VLM).
    public func getImageFeatures(_ pixelValues: MLXArray, gridH: Int, gridW: Int) -> MLXArray {
        var h = embeddings(pixelValues, gridH: gridH, gridW: gridW)
        h = encoder(h, gridH: gridH, gridW: gridW)
        h = postLayerNorm(h)
        return h
    }
}

// =============================================================================
// Plik: WhisperModel.swift
// Opis: Architektura Whispera w mlx-swift — port `whisper.py` z
//       ml-explore/mlx-examples 1:1, zeby klucze wag w safetensors zgadzaly
//       sie bez mappingu. Module hierarchy:
//
//         Whisper
//         ├── encoder: AudioEncoder
//         │   ├── conv1: Conv1d(n_mels, n_state, kernel=3, padding=1)
//         │   ├── conv2: Conv1d(n_state, n_state, kernel=3, stride=2, padding=1)
//         │   ├── blocks[0..n_audio_layer]: ResidualAttentionBlock
//         │   │   ├── attn: WhisperAttention
//         │   │   ├── attn_ln: LayerNorm
//         │   │   ├── mlp: Sequential(Linear, GELU, Linear)
//         │   │   └── mlp_ln: LayerNorm
//         │   └── ln_post: LayerNorm
//         └── decoder: TextDecoder
//             ├── token_embedding: Embedding
//             ├── positional_embedding: MLXArray (uczone)
//             ├── blocks[0..n_text_layer]: ResidualAttentionBlock(crossAttention=true)
//             │   ├── attn: WhisperAttention            (self, masked)
//             │   ├── attn_ln: LayerNorm
//             │   ├── cross_attn: WhisperAttention      (cross do encodera)
//             │   ├── cross_attn_ln: LayerNorm
//             │   ├── mlp: Sequential(Linear, GELU, Linear)
//             │   └── mlp_ln: LayerNorm
//             └── ln: LayerNorm
//
//       Forward i KV-cache zgodne z layoutem whisper.py — w step 1 piszemy
//       sam graf obliczeniowy, peTle dekodujaca przyjdzie w step 3.
// =============================================================================

import Foundation
import MLX
import MLXNN

/// Sinusoidalne embeddingi pozycyjne dla encodera audio. Whisper trzyma je
/// jako bufor (nie parametr uczony) — generowane przy starcie zgodnie z
/// `sinusoids()` z whisper.py. `length=1500` dla wszystkich rozmiarow.
private func sinusoids(length: Int, channels: Int, maxTimescale: Float = 10_000.0) -> MLXArray {
    precondition(channels % 2 == 0, "channels musi byc parzyste dla sinusoid")
    let logTimescaleIncrement = log(maxTimescale) / Float(channels / 2 - 1)
    let invTimescales = MLX.exp(
        -logTimescaleIncrement * MLXArray(0 ..< (channels / 2)).asType(.float32)
    )
    let pos = MLXArray(0 ..< length).asType(.float32)
    let scaled = pos.expandedDimensions(axis: 1) * invTimescales.expandedDimensions(axis: 0)
    return MLX.concatenated([MLX.sin(scaled), MLX.cos(scaled)], axis: 1)
}

/// Multi-head attention w stylu Whispera. Rozni sie od `MLXNN.MultiHeadAttention`
/// trzema rzeczami:
///   1. nazwy projekcji (`query`/`key`/`value`/`out` zamiast `*_proj`),
///   2. `key` jest bez biasu (Whisper specific — patrz Python whisper.py),
///   3. obsluguje cross-attention (kv z innego wejscia) i KV-cache w jednym call.
/// Te trzy rzeczy razem powoduja, ze nie da sie uzyc gotowego MHA z mlx-swift
/// bez przemapowywania wag — wlasna implementacja jest prostsza.
public final class WhisperAttention: Module {
    public let nHead: Int

    @ModuleInfo public var query: Linear
    @ModuleInfo public var key: Linear
    @ModuleInfo public var value: Linear
    @ModuleInfo public var out: Linear

    public init(nState: Int, nHead: Int) {
        self.nHead = nHead
        self._query.wrappedValue = Linear(nState, nState, bias: true)
        self._key.wrappedValue = Linear(nState, nState, bias: false)
        self._value.wrappedValue = Linear(nState, nState, bias: true)
        self._out.wrappedValue = Linear(nState, nState, bias: true)
    }

    /// Forward attention. `xa` != nil → cross-attention (k,v z encoder output).
    /// `mask` opcjonalny — uzywany tylko dla self-attention w decoderze
    /// (causal mask). Step 1: bez KV-cache, czysta wersja referencyjna.
    public func callAsFunction(
        _ x: MLXArray,
        xa: MLXArray? = nil,
        mask: MLXArray? = nil
    ) -> MLXArray {
        let q = query(x)
        let k = key(xa ?? x)
        let v = value(xa ?? x)

        let nBatch = q.dim(0)
        let nCtxQ = q.dim(1)
        let nCtxKV = k.dim(1)
        let nState = q.dim(2)
        let headDim = nState / nHead
        let scale = 1.0 / sqrt(Float(headDim))

        // (B, T, n_state) -> (B, n_head, T, head_dim)
        let qH = q.reshaped(nBatch, nCtxQ, nHead, headDim).transposed(0, 2, 1, 3)
        let kH = k.reshaped(nBatch, nCtxKV, nHead, headDim).transposed(0, 2, 1, 3)
        let vH = v.reshaped(nBatch, nCtxKV, nHead, headDim).transposed(0, 2, 1, 3)

        // scaled_dot_product_attention z mlx-swift dziala szybciej (Metal kernel)
        // niz recznie zrobione qk^T / softmax / v — i zaakceptuje opcjonalna
        // mask: (1, 1, nCtxQ, nCtxKV) z -inf w pozycjach do zamaskowania.
        let attn = MLXFast.scaledDotProductAttention(
            queries: qH, keys: kH, values: vH,
            scale: scale, mask: mask
        )

        // (B, n_head, T, head_dim) -> (B, T, n_state)
        let merged = attn.transposed(0, 2, 1, 3).reshaped(nBatch, nCtxQ, nState)
        return out(merged)
    }
}

/// MLP w stylu Whispera: dwie warstwy Linear z GELU pomiedzy nimi. Python
/// uzywa `nn.Sequential(Linear, GELU, Linear)` co daje klucze wag
/// `mlp.layers.0.weight` / `mlp.layers.2.weight`. mlx-swift `Sequential.layers`
/// nie ma `@ModuleInfo`, wiec `update(parameters:)` nie potrafi nim podmienic
/// wag. Mamy wlasna klase z polami `fc1`/`fc2`, a klucze safetensors loader
/// mapuje `layers.{0,2}` → `{fc1, fc2}` przed wlaniem.
public final class WhisperMLP: Module, UnaryLayer {
    @ModuleInfo public var fc1: Linear
    @ModuleInfo public var fc2: Linear

    public init(nState: Int) {
        let nMlp = nState * 4
        self._fc1.wrappedValue = Linear(nState, nMlp, bias: true)
        self._fc2.wrappedValue = Linear(nMlp, nState, bias: true)
    }

    public func callAsFunction(_ x: MLXArray) -> MLXArray {
        return fc2(MLXNN.gelu(fc1(x)))
    }
}

/// Pojedynczy blok residual Whispera. `crossAttention=true` w decoderze,
/// false w encoderze.
public final class ResidualAttentionBlock: Module {
    @ModuleInfo public var attn: WhisperAttention
    @ModuleInfo(key: "attn_ln") public var attnLn: LayerNorm

    @ModuleInfo(key: "cross_attn") public var crossAttn: WhisperAttention?
    @ModuleInfo(key: "cross_attn_ln") public var crossAttnLn: LayerNorm?

    @ModuleInfo public var mlp: WhisperMLP
    @ModuleInfo(key: "mlp_ln") public var mlpLn: LayerNorm

    public init(nState: Int, nHead: Int, crossAttention: Bool) {
        self._attn.wrappedValue = WhisperAttention(nState: nState, nHead: nHead)
        self._attnLn.wrappedValue = LayerNorm(dimensions: nState)
        if crossAttention {
            self._crossAttn.wrappedValue = WhisperAttention(nState: nState, nHead: nHead)
            self._crossAttnLn.wrappedValue = LayerNorm(dimensions: nState)
        } else {
            self._crossAttn.wrappedValue = nil
            self._crossAttnLn.wrappedValue = nil
        }
        self._mlp.wrappedValue = WhisperMLP(nState: nState)
        self._mlpLn.wrappedValue = LayerNorm(dimensions: nState)
    }

    public func callAsFunction(
        _ x: MLXArray,
        xa: MLXArray? = nil,
        mask: MLXArray? = nil
    ) -> MLXArray {
        var h = x + attn(attnLn(x), mask: mask)
        if let crossAttn, let crossAttnLn, let xa {
            h = h + crossAttn(crossAttnLn(h), xa: xa)
        }
        h = h + mlp(mlpLn(h))
        return h
    }
}

/// Encoder audio Whispera: dwie warstwy konwolucji 1D (downsampling 2x)
/// + dodanie sinusoidalnych embeddingow + N rezydualnych blokow self-attn
/// + final LayerNorm. Wejscie: log-mel spectrogram `(B, n_mels, n_frames)`.
public final class AudioEncoder: Module {
    @ModuleInfo public var conv1: Conv1d
    @ModuleInfo public var conv2: Conv1d
    @ModuleInfo public var blocks: [ResidualAttentionBlock]
    @ModuleInfo(key: "ln_post") public var lnPost: LayerNorm

    private let positionalEmbedding: MLXArray

    public init(nMels: Int, nCtx: Int, nState: Int, nHead: Int, nLayer: Int) {
        self._conv1.wrappedValue = Conv1d(inputChannels: nMels, outputChannels: nState, kernelSize: 3, padding: 1)
        self._conv2.wrappedValue = Conv1d(inputChannels: nState, outputChannels: nState, kernelSize: 3, stride: 2, padding: 1)
        self._blocks.wrappedValue = (0 ..< nLayer).map { _ in
            ResidualAttentionBlock(nState: nState, nHead: nHead, crossAttention: false)
        }
        self._lnPost.wrappedValue = LayerNorm(dimensions: nState)
        // Sinusoidy obliczone raz przy init — zgodnie z whisper.py NIE laduja
        // sie z safetensors, sa traktowane jako bufor (nie parametr).
        self.positionalEmbedding = sinusoids(length: nCtx, channels: nState)
    }

    /// Forward. `mel` ma ksztalt `(B, n_mels, n_frames=3000)` dla 30s audio
    /// przy hop 10 ms; po dwoch warstwach Conv1d (druga ze stride=2) wychodzi
    /// `(B, n_state, n_audio_ctx=1500)` ktore transpozycjonujemy na (B, T, C).
    public func callAsFunction(_ mel: MLXArray) -> MLXArray {
        // mlx-swift Conv1d oczekuje wejscia `(B, T, C_in)` — mel wchodzi jako
        // `(B, n_mels, T)`, wiec transpozycjonujemy na `(B, T, n_mels)` przed
        // konwolucja. To samo robi Pythonowy `whisper.py` w `mx.swapaxes`.
        var x = mel.transposed(0, 2, 1)
        x = MLXNN.gelu(conv1(x))
        x = MLXNN.gelu(conv2(x))
        // x: (B, T_out, n_state). Dodajemy sinusoidy na osi czasu.
        x = x + positionalEmbedding[0 ..< x.dim(1)]
        for block in blocks {
            x = block(x)
        }
        return lnPost(x)
    }
}

/// Decoder tekstu Whispera. `positional_embedding` jest UCZONA macierza,
/// nie sinusoidalna (rozni sie od encodera). `_mask` to causal mask
/// rozmiaru `(n_text_ctx, n_text_ctx)` z -inf nad diagonala.
public final class TextDecoder: Module {
    @ModuleInfo(key: "token_embedding") public var tokenEmbedding: Embedding
    @ModuleInfo public var blocks: [ResidualAttentionBlock]
    @ModuleInfo(key: "ln") public var ln: LayerNorm

    /// Uczona macierz embeddingow pozycyjnych; trafia tutaj z safetensors
    /// pod kluczem `decoder.positional_embedding`. Trzymamy jako parametr
    /// modulu (`@ParameterInfo`) zeby `update(parameters:)` ja podmienial.
    @ParameterInfo(key: "positional_embedding") public var positionalEmbedding: MLXArray

    private let mask: MLXArray

    public init(nVocab: Int, nCtx: Int, nState: Int, nHead: Int, nLayer: Int) {
        self._tokenEmbedding.wrappedValue = Embedding(embeddingCount: nVocab, dimensions: nState)
        self._blocks.wrappedValue = (0 ..< nLayer).map { _ in
            ResidualAttentionBlock(nState: nState, nHead: nHead, crossAttention: true)
        }
        self._ln.wrappedValue = LayerNorm(dimensions: nState)
        self._positionalEmbedding.wrappedValue = MLXArray.zeros([nCtx, nState])
        // Causal mask: gora-prawy trojkat ustawiony na -inf, reszta 0.
        // mlx-swift ma helper `MultiHeadAttention.createAdditiveCausalMask`
        // ktory zwraca dokladnie taki sam ksztalt jak whisper.py.
        self.mask = MultiHeadAttention.createAdditiveCausalMask(nCtx)
    }

    /// Forward. `tokens` o ksztalcie `(B, T)`; `xa` to encoder output
    /// `(B, n_audio_ctx, n_state)`. Zwraca logits `(B, T, n_vocab)` — Whisper
    /// dzieli wagi `token_embedding` z projekcja wyjsciowa przez `as_strided`
    /// w Pythonie, w Swifcie zrobimy to samo przez `tokenEmbedding.asLinear`.
    public func callAsFunction(_ tokens: MLXArray, xa: MLXArray) -> MLXArray {
        let nCtxQ = tokens.dim(1)
        var x = tokenEmbedding(tokens) + positionalEmbedding[0 ..< nCtxQ]
        // Mask trzeba przyciac do biezacego nCtxQ — full mask jest (nTextCtx, nTextCtx).
        let activeMask = mask[0 ..< nCtxQ, 0 ..< nCtxQ]
        for block in blocks {
            x = block(x, xa: xa, mask: activeMask)
        }
        x = ln(x)
        // Tied output projection: logits = x @ token_embedding.T.
        // Embedding ma `asLinear()` ktory wewnetrznie robi `x @ weight.T`.
        return tokenEmbedding.asLinear(x)
    }
}

/// Top-level model Whispera. Po prostu owija encoder + decoder.
public final class Whisper: Module {
    @ModuleInfo public var encoder: AudioEncoder
    @ModuleInfo public var decoder: TextDecoder

    public let config: WhisperConfig

    public init(config: WhisperConfig) {
        self.config = config
        self._encoder.wrappedValue = AudioEncoder(
            nMels: config.nMels,
            nCtx: config.nAudioCtx,
            nState: config.nAudioState,
            nHead: config.nAudioHead,
            nLayer: config.nAudioLayer
        )
        self._decoder.wrappedValue = TextDecoder(
            nVocab: config.nVocab,
            nCtx: config.nTextCtx,
            nState: config.nTextState,
            nHead: config.nTextHead,
            nLayer: config.nTextLayer
        )
    }
}

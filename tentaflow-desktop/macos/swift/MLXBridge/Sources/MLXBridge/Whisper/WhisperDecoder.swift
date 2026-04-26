// =============================================================================
// Plik: WhisperDecoder.swift
// Opis: Greedy decoder Whispera. Nie implementuje (jeszcze) temperature
//       fallback, beam search, ani word timestamps — celem MVP jest dzialajaca
//       transkrypcja PL/EN porownywalna jakosciowo z whisper.cpp na tym
//       samym audio. Dodatki przyjda po pierwszej weryfikacji numerycznej.
//
//       Optymalizacje:
//         - Cross-attention K/V z encodera liczone JEDEN raz (nie zaleza od
//           pozycji w decoderze, tylko od audio).
//         - Self-attention KV-cache budowany inkrementalnie — dla T-tego
//           tokena podajemy do dekodera tylko ostatni token, a poprzednie
//           K/V sa juz scache'owane w `WhisperDecoderState`.
// =============================================================================

import Foundation
import Compression
import MLX
import MLXNN
import MLXRandom

/// Stan pojedynczej sesji dekodowania — trzyma KV-cache self-attention dla
/// kazdej warstwy oraz cross-attention K/V dla kazdej warstwy (cross liczone
/// raz, kopia trzymana caly czas).
public final class WhisperDecoderState {
    /// Per warstwa: skumulowane key/value self-attention `(B, n_head, T_so_far, head_dim)`.
    /// nil dopoki pierwszy token nie zostal podany.
    public var selfK: [MLXArray?]
    public var selfV: [MLXArray?]
    /// Per warstwa: K/V cross-attention `(B, n_head, n_audio_ctx, head_dim)`.
    /// Liczone raz w `prepare(...)`.
    public var crossK: [MLXArray]
    public var crossV: [MLXArray]
    /// Cache encoder output (potrzebne tylko jesli kiedys dodamy ponowne
    /// dekodowanie z innym promptem).
    public var encoded: MLXArray

    public init(nLayer: Int, encoded: MLXArray, crossK: [MLXArray], crossV: [MLXArray]) {
        self.selfK = [MLXArray?](repeating: nil, count: nLayer)
        self.selfV = [MLXArray?](repeating: nil, count: nLayer)
        self.crossK = crossK
        self.crossV = crossV
        self.encoded = encoded
    }

    /// Plytka kopia — wszystkie MLXArray to immutable referencje (nowy K/V
    /// po `step()` jest osobnym tensorem dzieki `concatenated`), wiec
    /// kopiowanie struktury jest bezpieczne. Beam search uzywa tego do
    /// rozszczepienia stanu na N beamow bez duplikowania pamieci az do
    /// pierwszej rozbieznosci tokena.
    public func clone() -> WhisperDecoderState {
        let copy = WhisperDecoderState(
            nLayer: selfK.count,
            encoded: encoded,
            crossK: crossK,
            crossV: crossV
        )
        copy.selfK = selfK
        copy.selfV = selfV
        return copy
    }
}

public enum WhisperDecoder {
    /// Pre-oblicza encoder output i cross-attention K/V dla wszystkich warstw
    /// decodera. Zwraca stan ktory karmimy do `step(...)`.
    public static func prepare(model: Whisper, mel: MLXArray) -> WhisperDecoderState {
        // mel: (n_mels, T) -> (1, n_mels, T) batchify.
        let melBatched = mel.expandedDimensions(axis: 0)
        let encoded = model.encoder(melBatched)
        eval(encoded)

        // Dla cross-attention key/value to projekcje na encoder output. Liczymy
        // raz dla kazdej warstwy decodera, splaszczamy na heady (B, n_head, T, head_dim).
        let nHead = model.config.nTextHead
        let headDim = model.config.textHeadDim
        let nLayer = model.config.nTextLayer
        let nCtxKV = encoded.dim(1)

        var crossK: [MLXArray] = []
        var crossV: [MLXArray] = []
        crossK.reserveCapacity(nLayer)
        crossV.reserveCapacity(nLayer)
        for block in model.decoder.blocks {
            guard let cross = block.crossAttn, let crossLn = block.crossAttnLn else {
                fatalError("Decoder block bez cross-attention — niemozliwe dla Whispera")
            }
            // crossAttnLn dziala na hidden state decodera — tutaj nas interesuje
            // TYLKO transformacja encoder output przez key/value projection.
            // LayerNorm na xa nie jest stosowany (whisper.py robi to samo).
            _ = crossLn  // lint: zachowane dla parity z modelem
            let k = cross.key(encoded)
            let v = cross.value(encoded)
            let kH = k.reshaped(1, nCtxKV, nHead, headDim).transposed(0, 2, 1, 3)
            let vH = v.reshaped(1, nCtxKV, nHead, headDim).transposed(0, 2, 1, 3)
            crossK.append(kH)
            crossV.append(vH)
        }
        eval(crossK)
        eval(crossV)

        return WhisperDecoderState(
            nLayer: nLayer,
            encoded: encoded,
            crossK: crossK,
            crossV: crossV
        )
    }

    /// Pojedynczy krok dekodera — przyjmuje JEDEN token (`tokenId`) i pozycje
    /// (offset wzgledem poczatku sekwencji), zwraca logits dla nastepnego tokena.
    /// Aktualizuje KV-cache w `state` in-place.
    public static func step(
        model: Whisper,
        state: WhisperDecoderState,
        tokenId: Int,
        position: Int
    ) -> MLXArray {
        let nHead = model.config.nTextHead
        let headDim = model.config.textHeadDim
        let nState = model.config.nTextState

        // Embedding pojedynczego tokena + pozycyjny.
        let tokens = MLXArray([Int32(tokenId)]).reshaped(1, 1)
        var x = model.decoder.tokenEmbedding(tokens)
        x = x + model.decoder.positionalEmbedding[position ..< (position + 1)]

        for (idx, block) in model.decoder.blocks.enumerated() {
            // --- Self-attention z KV-cache ---
            let xLn = block.attnLn(x)
            let q = block.attn.query(xLn)
            let kNew = block.attn.key(xLn)
            let vNew = block.attn.value(xLn)

            let qH = q.reshaped(1, 1, nHead, headDim).transposed(0, 2, 1, 3)
            let kNewH = kNew.reshaped(1, 1, nHead, headDim).transposed(0, 2, 1, 3)
            let vNewH = vNew.reshaped(1, 1, nHead, headDim).transposed(0, 2, 1, 3)

            let kAll: MLXArray
            let vAll: MLXArray
            if let prevK = state.selfK[idx], let prevV = state.selfV[idx] {
                kAll = MLX.concatenated([prevK, kNewH], axis: 2)
                vAll = MLX.concatenated([prevV, vNewH], axis: 2)
            } else {
                kAll = kNewH
                vAll = vNewH
            }
            state.selfK[idx] = kAll
            state.selfV[idx] = vAll

            // Brak maski — q ma dlugosc 1, wiec w cache widzi tylko poprzednie
            // i samego siebie, co JEST poprawnym causal'em.
            let scale = 1.0 / sqrt(Float(headDim))
            let selfAttn = MLXFast.scaledDotProductAttention(
                queries: qH, keys: kAll, values: vAll,
                scale: scale, mask: nil
            )
            let selfMerged = selfAttn.transposed(0, 2, 1, 3).reshaped(1, 1, nState)
            x = x + block.attn.out(selfMerged)

            // --- Cross-attention z prekomputowanym K/V ---
            guard let cross = block.crossAttn, let crossLn = block.crossAttnLn else {
                fatalError("Decoder block bez cross-attn — niemozliwe")
            }
            let xCrossLn = crossLn(x)
            let qC = cross.query(xCrossLn)
            let qCH = qC.reshaped(1, 1, nHead, headDim).transposed(0, 2, 1, 3)
            let crossAttn = MLXFast.scaledDotProductAttention(
                queries: qCH, keys: state.crossK[idx], values: state.crossV[idx],
                scale: scale, mask: nil
            )
            let crossMerged = crossAttn.transposed(0, 2, 1, 3).reshaped(1, 1, nState)
            x = x + cross.out(crossMerged)

            // --- MLP ---
            x = x + block.mlp(block.mlpLn(x))
        }
        x = model.decoder.ln(x)
        // Logity: x @ token_embedding.T -> (1, 1, n_vocab)
        let logits = model.decoder.tokenEmbedding.asLinear(x)
        // Zwracamy ksztalt (n_vocab,) zeby sampling byl trywialny.
        return logits.squeezed(axes: [0, 1])
    }

    /// Wynik transkrypcji jednego okna — tokeny + offset czasowy w sekundach
    /// gdzie konczy sie ostatni TIMESTAMP (lub `nil` gdy zaden timestamp nie
    /// pojawil sie) + statystyki jakosci do temperature fallback.
    public struct WindowResult {
        public let tokens: [Int]
        public let lastTimestampSeconds: Double?
        /// Sredni log-prob wybranego tokena (po log-softmax). Niskie wartosci
        /// (< -1.0) sygnalizuja ze model byl niepewny — kandydat do retry.
        public let avgLogprob: Double
        /// Wspolczynnik kompresji `tokens.count / zlib(text).count`. > 2.4
        /// = "in in in in" -> retry z wyzsza temperatura.
        public let compressionRatio: Double
        /// Prawdopodobienstwo `<|nospeech|>` na pierwszej pozycji decodera —
        /// jezeli > 0.6 odrzucamy okno jako cisze.
        public let noSpeechProb: Double
    }

    /// Pelna petla transkrypcji dla pojedynczego okna (max 30s, dane juz
    /// zapaddowane). Whisper emituje tokeny timestampu `<|N.NN|>` co 20 ms;
    /// trzymamy ich pozycje aby caller mogl seekowac w dlugim audio bez
    /// gubienia slow na granicy okna.
    /// Beam search Whispera. Implementuje standardowy algorytm: kazdy beam
    /// w kazdym kroku rozszerza sie o top-K nastepnych tokenow, globalnie
    /// zostawiamy najlepsze `beamSize` kandydatow wedlug skumulowanego
    /// log-prob. Stop gdy WSZYSTKIE beamy uderzyly w `<|endoftext|>` lub
    /// osiagniety zostal `maxTokens`.
    ///
    /// Whisper paper i mlx-examples uzywaja domyslnie `beam_size=5` — to
    /// najlepszy kompromis miedzy jakoscia a kosztem (~5x wolniej niz
    /// greedy, +1-2 pp na realnej mowie).
    public static func beamSearch(
        model: Whisper,
        tokenizer: WhisperTokenizer,
        mel: MLXArray,
        language: String,
        beamSize: Int = 5,
        maxTokens: Int = 224
    ) -> WindowResult {
        // Beam: (skumulowany log-prob, ciag wygenerowanych tokenow, stan
        // dekodera, czy beam juz zamknieto eot, ostatnie logits).
        struct Beam {
            var score: Double
            var tokens: [Int]
            var state: WhisperDecoderState
            var finished: Bool
            // logits do wybrania kolejnego tokena. Beamy ZAMKNIETE (finished)
            // maja `nil` — nie sa juz rozwijane.
            var lastLogits: MLXArray?
        }
        let initialState = prepare(model: model, mel: mel)
        let langTok = tokenizer.languageTokens[language] ?? tokenizer.languageTokens["en"]!
        let startSeq = [tokenizer.sotToken, langTok, tokenizer.transcribeToken]
        var firstLogits: MLXArray? = nil
        var noSpeechProb: Double = 0.0
        for (i, tok) in startSeq.enumerated() {
            firstLogits = step(model: model, state: initialState, tokenId: tok, position: i)
            if i == 0, let l = firstLogits {
                eval(l)
                let probs = MLX.softmax(l, axis: -1)
                noSpeechProb = Double(probs[tokenizer.noSpeechToken].item(Float.self))
            }
        }
        // Wszystkie beamy startuja od tego samego stanu — dopiero pierwsza
        // ekspansja wytwarza beamSize gałęzi.
        var beams: [Beam] = [
            Beam(
                score: 0.0,
                tokens: [],
                state: initialState,
                finished: false,
                lastLogits: firstLogits
            )
        ]
        let timestampBase = tokenizer.timestampBase

        for stepIdx in 0 ..< maxTokens {
            var candidates: [Beam] = []
            for beam in beams {
                if beam.finished {
                    candidates.append(beam)
                    continue
                }
                guard let logits = beam.lastLogits else { continue }
                eval(logits)
                let logProbs = logits - MLX.logSumExp(logits, axis: -1, keepDims: true)
                // top-K indeksow przez argSort + slice. Wektor (V,) mlx-swift
                // potrzebuje axis dla argSort.
                let sorted = argSort(logProbs, axis: -1)
                let nVocab = logProbs.dim(0)
                let topK = min(beamSize, nVocab)
                // Slice ostatnich K indeksow w odwrotnej kolejnosci (od
                // najwiekszego). Skopiowanie do Swift Int — mlx-swift nie ma
                // gather po MLXArray na indeksach skalarnych.
                let topIdxArr = sorted[(nVocab - topK) ..< nVocab]
                eval(topIdxArr)
                let topIdx: [Int] = (0 ..< topK).map { i in
                    Int(topIdxArr[topK - 1 - i].item(Int32.self))
                }
                for nextTok in topIdx {
                    let lp = Double(logProbs[nextTok].item(Float.self))
                    var childState = beam.state.clone()
                    let position = startSeq.count + beam.tokens.count
                    let nextLogits: MLXArray? = (nextTok == tokenizer.eotToken)
                        ? nil
                        : step(model: model, state: childState, tokenId: nextTok, position: position)
                    candidates.append(Beam(
                        score: beam.score + lp,
                        tokens: beam.tokens + [nextTok],
                        state: childState,
                        finished: nextTok == tokenizer.eotToken,
                        lastLogits: nextLogits
                    ))
                }
            }
            // Globalny top beamSize wedlug skumulowanego log-prob. Whisper
            // domyslnie nie stosuje length penalty na beamach jeszcze otwartych.
            candidates.sort { $0.score > $1.score }
            beams = Array(candidates.prefix(beamSize))
            // Wszystkie beamy zamkniete -> stop.
            if beams.allSatisfy({ $0.finished }) { break }
            // Bezpiecznik na limit kontekstu decodera.
            if startSeq.count + stepIdx + 1 >= model.config.nTextCtx { break }
        }
        // Wybor finalisty: najwyzszy score (po length-normalizacji prostej —
        // dlugie ciagi maja nizszy log-prob, ale Whisper paper pozostawia
        // alpha=0 i polega na compression_ratio gating dla katastroficznych
        // przypadkow halucynacji).
        let winner = beams.max(by: { $0.score < $1.score }) ?? beams[0]
        let avgLogprob = winner.tokens.isEmpty ? -10.0 : winner.score / Double(winner.tokens.count)
        var lastTimestamp: Double? = nil
        for tok in winner.tokens.reversed() {
            if tok >= timestampBase {
                lastTimestamp = Double(tok - timestampBase) * 0.02
                break
            }
        }
        let text = tokenizer.decode(tokens: winner.tokens)
        return WindowResult(
            tokens: winner.tokens,
            lastTimestampSeconds: lastTimestamp,
            avgLogprob: avgLogprob,
            compressionRatio: computeCompressionRatio(text),
            noSpeechProb: noSpeechProb
        )
    }

    /// Pojedynczy "decode pass" przy zadanej temperaturze. T=0 → argmax
    /// (deterministyczne, najszybsze). T>0 → losowanie kategoryczne z logitow
    /// podzielonych przez T (whisper.py temperature fallback).
    public static func decodeOnce(
        model: Whisper,
        tokenizer: WhisperTokenizer,
        mel: MLXArray,
        language: String,
        temperature: Float,
        maxTokens: Int
    ) -> WindowResult {
        let state = prepare(model: model, mel: mel)
        let langTok = tokenizer.languageTokens[language] ?? tokenizer.languageTokens["en"]!
        let startSeq = [
            tokenizer.sotToken,
            langTok,
            tokenizer.transcribeToken,
        ]
        var lastLogits: MLXArray? = nil
        var noSpeechProb: Double = 0.0
        for (i, tok) in startSeq.enumerated() {
            lastLogits = step(model: model, state: state, tokenId: tok, position: i)
            // Po pierwszym kroku (po <|sot|>) dystrybucja na nastepny token
            // ma masowy udzial `<|nospeech|>` jezeli to bylo cicho. Mierzymy
            // tu — odpowiada to whisper.py `decoding.detect_language()` /
            // `_get_no_speech_probs()`.
            if i == 0, let l = lastLogits {
                eval(l)
                let probs = MLX.softmax(l, axis: -1)
                noSpeechProb = Double(probs[tokenizer.noSpeechToken].item(Float.self))
            }
        }
        var generated: [Int] = []
        var sumLogprob: Double = 0.0
        var pos = startSeq.count
        let timestampBase = tokenizer.timestampBase

        for _ in 0 ..< maxTokens {
            guard let logits = lastLogits else { break }
            eval(logits)
            let nextTok: Int
            if temperature == 0 {
                nextTok = argMax(logits, axis: -1).item(Int.self)
            } else {
                let scaled = logits / temperature
                nextTok = MLXRandom.categorical(scaled).item(Int.self)
            }
            // Akumulacja log-probability wybranego tokena (na potrzeby
            // avg_logprob fallback decision).
            let logProbs = logits - MLX.logSumExp(logits, axis: -1, keepDims: true)
            sumLogprob += Double(logProbs[nextTok].item(Float.self))

            if nextTok == tokenizer.eotToken { break }
            generated.append(nextTok)
            lastLogits = step(model: model, state: state, tokenId: nextTok, position: pos)
            pos += 1
            if pos >= model.config.nTextCtx { break }
        }

        var lastTimestamp: Double? = nil
        for tok in generated.reversed() {
            if tok >= timestampBase {
                lastTimestamp = Double(tok - timestampBase) * 0.02
                break
            }
        }
        let avgLogprob = generated.isEmpty ? -10.0 : sumLogprob / Double(generated.count)
        let text = tokenizer.decode(tokens: generated)
        let compressionRatio = computeCompressionRatio(text)
        return WindowResult(
            tokens: generated,
            lastTimestampSeconds: lastTimestamp,
            avgLogprob: avgLogprob,
            compressionRatio: compressionRatio,
            noSpeechProb: noSpeechProb
        )
    }

    /// Whisper "robust" transcribe: pierwsza proba beam search (T=0,
    /// beamSize=5), kolejne T=0.2, 0.4, ..., 1.0 sampling jezeli
    /// `compression_ratio > 2.4` LUB `avg_logprob < -1.0`. Dokladny algorytm
    /// z whisper.py `_decode_with_fallback`.
    public static func transcribe(
        model: Whisper,
        tokenizer: WhisperTokenizer,
        mel: MLXArray,
        language: String,
        maxTokens: Int = 224,
        beamSize: Int = 5
    ) -> WindowResult {
        let compressionThreshold = 2.4
        let logprobThreshold = -1.0
        var best: WindowResult? = nil
        // Etap 1: beam search T=0. Dla 99% audio to jedyna potrzebna proba.
        let beamResult = beamSearch(
            model: model, tokenizer: tokenizer, mel: mel,
            language: language, beamSize: beamSize, maxTokens: maxTokens
        )
        best = beamResult
        let beamOK = beamResult.compressionRatio <= compressionThreshold
            && beamResult.avgLogprob >= logprobThreshold
        if beamOK {
            return beamResult
        }
        print("[MLXWhisper] beam search slabe (compression=\(String(format: "%.2f", beamResult.compressionRatio)) logprob=\(String(format: "%.2f", beamResult.avgLogprob))), fallback na sampling")
        // Etap 2: sampling z rosnaca temperatura.
        for t in stride(from: Float(0.2), through: Float(1.0), by: 0.2) {
            let result = decodeOnce(
                model: model, tokenizer: tokenizer, mel: mel,
                language: language, temperature: t, maxTokens: maxTokens
            )
            // Trzymamy ten z najlepszym avgLogprob — whisper.py wybor "fallback'u"
            // ma byc "least bad" jezeli zaden nie spelnia warunkow.
            if let b = best, result.avgLogprob > b.avgLogprob {
                best = result
            }
            let needRetry = result.compressionRatio > compressionThreshold
                || result.avgLogprob < logprobThreshold
            if !needRetry {
                return result
            }
            print("[MLXWhisper] retry T=\(t) → next")
        }
        return best ?? WindowResult(
            tokens: [], lastTimestampSeconds: nil,
            avgLogprob: -10.0, compressionRatio: 0.0, noSpeechProb: 1.0
        )
    }

    /// `len(tokens_text_utf8) / len(zlib_compressed)`. Whisper.py uzywa
    /// dokladnie tego — wyzszy wynik = wieksza redundancja = halucynacja
    /// "in in in in...". Implementacja: zwykly count UTF-8 bytes / count
    /// po `Compression.zlib`.
    private static func computeCompressionRatio(_ text: String) -> Double {
        if text.isEmpty { return 0.0 }
        let raw = Data(text.utf8)
        guard let compressed = compressZlib(raw), !compressed.isEmpty else {
            return 0.0
        }
        return Double(raw.count) / Double(compressed.count)
    }

    /// Foundation `Compression` zlib opakowany w prosty helper. Dla tak
    /// krotkich stringow (do 1 KB) narzut jest nieistotny.
    private static func compressZlib(_ data: Data) -> Data? {
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Data? in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return nil }
            let dstCap = data.count + 64
            let dst = UnsafeMutablePointer<UInt8>.allocate(capacity: dstCap)
            defer { dst.deallocate() }
            let written = compression_encode_buffer(
                dst, dstCap, base, data.count, nil, COMPRESSION_ZLIB
            )
            if written == 0 { return nil }
            return Data(bytes: dst, count: written)
        }
    }
}


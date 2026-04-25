// =============================================================================
// Plik: MLXBridge.swift
// Opis: Mostek mlx-swift / MLXLLM dla TentaFlow desktop macOS. Adaptacja
//       tentaflow-mobile/ios/TentaFlowAI/MLXSwiftEngine.swift gdzie Bielik
//       4.5B 4-bit dziala bez zarzutu. Eksportuje cztery `@_cdecl` funkcje
//       ktorych Rust desktop bin uzywa do FFI registration:
//         MLXBridge_loadModel
//         MLXBridge_unloadModel
//         MLXBridge_generate
//         MLXBridge_modelInfo
//       Plus akcesor `MLXBridge_getContext` zwracajacy pointer na singleton
//       silnika — Rust przekazuje go jako `context` do callbackow.
// =============================================================================

import Foundation
import MLX
import MLXLLM
import MLXLMCommon

/// Silnik MLX na macOS — singleton zarzadzajacy modelem MLX i FFI callbacks.
public final class MLXBridgeEngine {
    /// Globalny singleton — Swift gwarantuje thread-safe lazy init.
    public static let shared = MLXBridgeEngine()

    private var modelContainer: ModelContainer?
    private var modelPath: String?

    private init() {
        // Limit cache GPU — wystarczy duzo dla M-series, ale nie bezgranicznie.
        // 256 MB dziala dobrze dla modeli 4-7B 4-bit; wieksze modele beda
        // korzystac z system memory through unified memory architecture.
        MLX.GPU.set(cacheLimit: 256 * 1024 * 1024)
    }

    /// Laduje model z podanej sciezki. Synchroniczne (blokuje watek wolajacy).
    public func loadModel(path: String) -> Bool {
        print("[MLXBridge] Ladowanie modelu: \(path)")

        let url = URL(filePath: path)
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXBridge] Sciezka nie istnieje: \(path)")
            return false
        }

        let semaphore = DispatchSemaphore(value: 0)
        var success = false

        Task {
            do {
                let config = ModelConfiguration(directory: url)
                self.modelContainer = try await LLMModelFactory.shared.loadContainer(
                    configuration: config
                ) { progress in
                    let pct = Int(progress.fractionCompleted * 100)
                    if pct % 25 == 0 {
                        print("[MLXBridge] Ladowanie: \(pct)%")
                    }
                }
                self.modelPath = path
                success = true
                print("[MLXBridge] Model zaladowany pomyslnie")
            } catch {
                print("[MLXBridge] Blad ladowania: \(error)")
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Wyladowuje model z pamieci.
    public func unloadModel() {
        print("[MLXBridge] Wyladowywanie modelu")
        modelContainer = nil
        modelPath = nil
    }

    /// Generuje tekst z callbackiem na każdy token. Synchroniczne.
    public func generate(
        prompt: String,
        maxTokens: Int,
        temperature: Float,
        topP: Float,
        tokenCallback: @escaping (String, Bool) -> Void
    ) -> Bool {
        guard let container = modelContainer else {
            print("[MLXBridge] Brak zaladowanego modelu")
            return false
        }

        print("[MLXBridge] Generowanie: max_tokens=\(maxTokens), temp=\(temperature), topP=\(topP)")
        print("[MLXBridge] Prompt (\(prompt.count) znakow): \(prompt.prefix(200))")

        let semaphore = DispatchSemaphore(value: 0)
        var success = false

        // Bielik 4-bit (i ogolnie male instruct modele) bez `repetitionPenalty`
        // wpadaja w pętle po 200+ tokenach: ten sam fragment ("Krotki Wiersz...")
        // generowany w nieskonczonosc. Conservative default 1.1 z context size
        // 20 jest standardem dla mlx-swift LLMEval i nie psuje koherencji.
        // (Default w `GenerateParameters` to nil = wyłączone, tylko iOS dziala
        // przy KROTKICH odpowiedziach gdzie pętla nie zdąży się zaczać.)
        let parameters = GenerateParameters(
            temperature: temperature,
            topP: topP,
            repetitionPenalty: 1.1,
            repetitionContextSize: 20
        )

        Task {
            do {
                let _ = try await container.perform { context in
                    // Prompt juz jest sformatowany przez Rust (ChatML lub Mistral).
                    // Tokenizujemy bezposrednio — bez processor.prepare.
                    let tokenIds = context.tokenizer.encode(text: prompt)
                    let inputTokens = MLXArray(tokenIds)
                    let input = LMInput(tokens: inputTokens)

                    // Stop tokeny ChatML (zgodnie z iOS).
                    let stopStrings = ["<|im_end|>", "<|endoftext|>", "</s>"]

                    var lastOutput = ""
                    return try MLXLMCommon.generate(
                        input: input,
                        parameters: parameters,
                        context: context
                    ) { tokens in
                        // Inkrementalny dekod — emituj tylko nowy fragment.
                        // chars-based diff (Swift String.count == char count) eliminuje
                        // problemy z UTF-8 boundary z polskimi znakami.
                        let fullText = context.tokenizer.decode(tokens: tokens)
                        if fullText.count > lastOutput.count {
                            let newPart = String(fullText.dropFirst(lastOutput.count))
                            tokenCallback(newPart, false)
                        }
                        lastOutput = fullText

                        for stop in stopStrings {
                            if fullText.contains(stop) {
                                return .stop
                            }
                        }
                        return tokens.count >= maxTokens ? .stop : .more
                    }
                }

                tokenCallback("", true)
                success = true
                print("[MLXBridge] Generowanie zakonczone")
            } catch {
                print("[MLXBridge] Blad generowania: \(error)")
                tokenCallback("", true)
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Zwraca JSON z info o zaladowanym modelu.
    public func modelInfo() -> String? {
        guard modelContainer != nil, let path = modelPath else { return nil }
        let name = URL(filePath: path).lastPathComponent
        let info: [String: Any] = [
            "name": name,
            "path": path,
            "backend": "mlx-swift",
            "loaded": true,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: info),
           let json = String(data: data, encoding: .utf8) {
            return json
        }
        return nil
    }
}

// =============================================================================
// C-ABI exports — Rust uzywa ich przez FFI w tentaflow_register_mlx_swift.
// Sygnatury MUSZA pasowac dokladnie do typow w
// tentaflow-core/src/inference/mlx_swift_bridge.rs.
// =============================================================================

/// Zwraca surowy pointer na singleton silnika. Rust przekazuje go jako
/// `context` do każdego z czterech callbacków poniżej.
@_cdecl("MLXBridge_getContext")
public func MLXBridge_getContext() -> UnsafeMutableRawPointer {
    return Unmanaged.passUnretained(MLXBridgeEngine.shared).toOpaque()
}

@_cdecl("MLXBridge_loadModel")
public func MLXBridge_loadModel(
    modelPath: UnsafePointer<CChar>?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

@_cdecl("MLXBridge_unloadModel")
public func MLXBridge_unloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

@_cdecl("MLXBridge_generate")
public func MLXBridge_generate(
    prompt: UnsafePointer<CChar>?,
    maxTokens: Int32,
    temperature: Float,
    topP: Float,
    tokenCallback: (@convention(c) (UnsafePointer<CChar>?, Bool, UnsafeMutableRawPointer?) -> Void)?,
    callbackContext: UnsafeMutableRawPointer?,
    context: UnsafeMutableRawPointer?
) -> Int32 {
    guard let promptStr = prompt.flatMap({ String(cString: $0) }),
          let ctx = context,
          let tokenCb = tokenCallback else { return -1 }

    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()

    let success = engine.generate(
        prompt: promptStr,
        maxTokens: Int(maxTokens),
        temperature: temperature,
        topP: topP
    ) { text, isFinal in
        text.withCString { cstr in
            tokenCb(cstr, isFinal, callbackContext)
        }
    }
    return success ? 0 : -1
}

@_cdecl("MLXBridge_modelInfo")
public func MLXBridge_modelInfo(
    context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard let ctx = context else { return nil }
    let engine = Unmanaged<MLXBridgeEngine>.fromOpaque(ctx).takeUnretainedValue()
    guard let json = engine.modelInfo() else { return nil }
    // strdup() alokuje przez malloc — Rust zwolni przez libc free().
    return strdup(json)
}

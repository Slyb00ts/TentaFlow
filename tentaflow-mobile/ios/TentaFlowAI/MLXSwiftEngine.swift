// =============================================================================
// Plik: MLXSwiftEngine.swift
// Opis: Natywny silnik inferencji MLX na iOS — wrapper na MLXLLM framework.
//       Rejestruje callbacks w Rust core przez FFI.
// =============================================================================

import Foundation
import MLX
import MLXLLM
import MLXLMCommon

/// Silnik MLX na iOS — ladowanie modeli i generowanie tekstu
class MLXSwiftEngine {
    static let shared = MLXSwiftEngine()

    private var modelContainer: ModelContainer?
    private var modelPath: String?
    private let queue = DispatchQueue(label: "ai.tentaflow.mlx", qos: .userInitiated)

    private init() {
        // Ustaw limit cache GPU — maly limit na iOS zeby nie wyczerpac pamieci
        MLX.GPU.set(cacheLimit: 64 * 1024 * 1024)
    }

    /// Rejestruje callbacks w Rust core
    func registerWithRust() {
        // Przekaz function pointers do Rust
        let context = Unmanaged.passUnretained(self).toOpaque()

        tentaflow_register_mlx_swift(
            swiftLoadModel,
            swiftUnloadModel,
            swiftGenerate,
            swiftModelInfo,
            context
        )
        print("[MLXSwift] Callbacks zarejestrowane w Rust")
    }

    /// Laduje model z podanej sciezki
    func loadModel(path: String) -> Bool {
        print("[MLXSwift] Ladowanie modelu: \(path)")

        let url = URL(filePath: path)

        // Sprawdz czy katalog istnieje
        guard FileManager.default.fileExists(atPath: path) else {
            print("[MLXSwift] Sciezka nie istnieje: \(path)")
            return false
        }

        // Zaladuj model synchronicznie (blokuje watek)
        let semaphore = DispatchSemaphore(value: 0)
        var success = false

        Task {
            do {
                let config = ModelConfiguration(directory: url)
                self.modelContainer = try await LLMModelFactory.shared.loadContainer(
                    configuration: config
                ) { progress in
                    if Int(progress.fractionCompleted * 100) % 25 == 0 {
                        print("[MLXSwift] Ladowanie: \(Int(progress.fractionCompleted * 100))%")
                    }
                }
                self.modelPath = path
                success = true
                print("[MLXSwift] Model zaladowany pomyslnie")
            } catch {
                print("[MLXSwift] Blad ladowania: \(error)")
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Wyladowuje model z pamieci
    func unloadModel() {
        print("[MLXSwift] Wyladowywanie modelu")
        modelContainer = nil
        modelPath = nil
    }

    /// Generuje tekst z callbackiem na kazdy token
    func generate(
        prompt: String,
        maxTokens: Int,
        temperature: Float,
        topP: Float,
        tokenCallback: @escaping (String, Bool) -> Void
    ) -> Bool {
        guard let container = modelContainer else {
            print("[MLXSwift] Brak zaladowanego modelu")
            return false
        }

        print("[MLXSwift] Generowanie: max_tokens=\(maxTokens), temp=\(temperature)")
        print("[MLXSwift] Prompt (\(prompt.count) znakow): \(prompt.prefix(200))")

        let semaphore = DispatchSemaphore(value: 0)
        var success = false

        let parameters = GenerateParameters(temperature: temperature, topP: topP)

        Task {
            do {
                let result = try await container.perform { context in
                    // Prompt juz jest sformatowany przez Rust (ChatML z <|im_start|> tokenami)
                    // Tokenizujemy bezposrednio — bez processor.prepare ktory probuje formatowac
                    let tokenIds = context.tokenizer.encode(text: prompt)
                    let inputTokens = MLXArray(tokenIds)
                    let input = LMInput(tokens: inputTokens)

                    // Stop tokeny — ChatML format
                    let stopStrings = ["<|im_end|>", "<|endoftext|>", "</s>"]

                    var lastOutput = ""
                    return try MLXLMCommon.generate(
                        input: input,
                        parameters: parameters,
                        context: context
                    ) { tokens in
                        // Dekoduj CALY tekst i wyslij roznice (inkrementalnie)
                        let fullText = context.tokenizer.decode(tokens: tokens)
                        if fullText.count > lastOutput.count {
                            let newPart = String(fullText.dropFirst(lastOutput.count))
                            tokenCallback(newPart, false)
                        }
                        lastOutput = fullText

                        // Sprawdz stop tokeny
                        for stop in stopStrings {
                            if fullText.contains(stop) {
                                return .stop
                            }
                        }

                        return tokens.count >= maxTokens ? .stop : .more
                    }
                }

                // Finalny callback
                tokenCallback("", true)
                success = true
                print("[MLXSwift] Generowanie zakonczone: \(result.output.count) znakow")
                print("[MLXSwift] Output: \(result.output.prefix(300))")
            } catch {
                print("[MLXSwift] Blad generowania: \(error)")
                tokenCallback("", true)
                success = false
            }
            semaphore.signal()
        }

        semaphore.wait()
        return success
    }

    /// Zwraca JSON z info o modelu
    func modelInfo() -> String? {
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
// C callbacks — wywolywane z Rust przez FFI
// =============================================================================

/// C callback: zaladuj model
private func swiftLoadModel(modelPath: UnsafePointer<CChar>?, context: UnsafeMutableRawPointer?) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }),
          let ctx = context else { return -1 }

    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()
    return engine.loadModel(path: path) ? 0 : -1
}

/// C callback: wyladuj model
private func swiftUnloadModel(context: UnsafeMutableRawPointer?) {
    guard let ctx = context else { return }
    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()
    engine.unloadModel()
}

/// C callback: generuj tekst
private func swiftGenerate(
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

    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()

    let success = engine.generate(
        prompt: promptStr,
        maxTokens: Int(maxTokens),
        temperature: temperature,
        topP: topP
    ) { text, isFinal in
        // Wywolaj Rust token callback
        text.withCString { cstr in
            tokenCb(cstr, isFinal, callbackContext)
        }
    }

    return success ? 0 : -1
}

/// C callback: info o modelu
private func swiftModelInfo(context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>? {
    guard let ctx = context else { return nil }
    let engine = Unmanaged<MLXSwiftEngine>.fromOpaque(ctx).takeUnretainedValue()

    guard let json = engine.modelInfo() else { return nil }

    // Alokuj C string — Rust musi go zwolnic przez free()
    return strdup(json)
}

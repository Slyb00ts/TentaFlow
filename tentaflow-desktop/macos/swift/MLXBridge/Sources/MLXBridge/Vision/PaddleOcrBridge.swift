// =============================================================================
// Plik: PaddleOcrBridge.swift
// Opis: Most cdecl do PaddleOCR-VL (vendored w PaddleOCR/) — parsing dokumentow
//       na MLX: OCR + struktura tabel + wzory + wykresy. Zadanie wybiera tryb
//       (ocr/table/formula/chart). Cdecl `MLXPaddleOCR_*` jest niezalezny od
//       LLM/Embed w MLXBridge — wlasny singleton pipeline'u. Rust laduje go przez
//       libloading z libMLXBridge.dylib (jak MLXAppleOCR / MLXBridge_embed).
// =============================================================================

import CoreImage
import Foundation

/// Singleton trzymajacy zaladowany pipeline PaddleOCR-VL. Dostep z watkow FFI
/// Rusta serializowany przez DispatchSemaphore w `load` (sam `recognize` jest
/// synchroniczny i liczony na watku wolajacym — Rust woła go w spawn_blocking).
public final class PaddleOcrBridgeEngine: @unchecked Sendable {
    public static let shared = PaddleOcrBridgeEngine()

    private var pipeline: PaddleOCRVLPipeline?

    private init() {
        // Odbuforuj stdout — printy [PaddleOCR] z dylibu (non-TTY) inaczej
        // siedza w buforze i nie pojawiaja sie w logu serwera az do flushu.
        setvbuf(stdout, nil, _IONBF, 0)
    }

    /// Maksymalny czas ladowania modelu (s). Po przekroczeniu zwracamy czysty
    /// blad zamiast blokowac watek deployu w nieskonczonosc (anty-hang).
    private static let loadTimeoutSec: Int = 300

    /// Laduje model z katalogu. Synchroniczne (blokuje watek wolajacy).
    public func load(path: String) -> Bool {
        print("[PaddleOCR] Ladowanie modelu: \(path)"); fflush(stdout)
        guard FileManager.default.fileExists(atPath: path) else {
            print("[PaddleOCR] Sciezka nie istnieje: \(path)"); fflush(stdout)
            return false
        }
        let semaphore = DispatchSemaphore(value: 0)
        var success = false
        Task {
            do {
                self.pipeline = try await PaddleOCRVLPipeline(modelPath: path)
                success = true
                print("[PaddleOCR] Model zaladowany"); fflush(stdout)
            } catch {
                print("[PaddleOCR] Blad ladowania: \(error)"); fflush(stdout)
                success = false
            }
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + .seconds(Self.loadTimeoutSec)) == .timedOut {
            print("[PaddleOCR] TIMEOUT ladowania (>\(Self.loadTimeoutSec)s) — przerywam"); fflush(stdout)
            return false
        }
        return success
    }

    public func unload() {
        pipeline = nil
    }

    /// Rozpoznaje obraz (PNG/JPEG) dla zadanego zadania. Zwraca tekst wyniku
    /// (dla `table` to struktura HTML/markdown, dla `formula` LaTeX) albo nil.
    public func recognize(imageData: Data, task: String) -> String? {
        guard let pipeline else {
            print("[PaddleOCR] Brak zaladowanego modelu")
            return nil
        }
        guard let image = CIImage(data: imageData) else {
            print("[PaddleOCR] Nie udalo sie zdekodowac obrazu")
            return nil
        }
        let resolved = PaddleOCRTask(rawValue: task) ?? .ocr
        return pipeline.recognize(image: image, task: resolved)
    }
}

// =============================================================================
// C-ABI exports — Rust uzywa ich przez libloading (inference-mlx-ocr).
// =============================================================================

@_cdecl("MLXPaddleOCR_load")
public func MLXPaddleOCR_load(modelPath: UnsafePointer<CChar>?) -> Int32 {
    guard let path = modelPath.flatMap({ String(cString: $0) }) else { return -1 }
    return PaddleOcrBridgeEngine.shared.load(path: path) ? 0 : -1
}

@_cdecl("MLXPaddleOCR_unload")
public func MLXPaddleOCR_unload() {
    PaddleOcrBridgeEngine.shared.unload()
}

/// Rozpoznaje obraz. `task` = "ocr"|"table"|"formula"|"chart". Zwraca C string
/// (malloc — Rust zwalnia przez libc free) albo NULL przy bledzie.
@_cdecl("MLXPaddleOCR_recognize")
public func MLXPaddleOCR_recognize(
    imageBytes: UnsafePointer<UInt8>?,
    imageLen: Int32,
    task: UnsafePointer<CChar>?
) -> UnsafeMutablePointer<CChar>? {
    guard let bytes = imageBytes, imageLen > 0 else { return nil }
    let data = Data(bytes: bytes, count: Int(imageLen))
    let taskStr = task.flatMap { String(cString: $0) } ?? "ocr"
    guard let text = PaddleOcrBridgeEngine.shared.recognize(imageData: data, task: taskStr) else {
        return nil
    }
    // strdup() alokuje przez malloc — Rust zwolni przez libc free().
    return strdup(text)
}

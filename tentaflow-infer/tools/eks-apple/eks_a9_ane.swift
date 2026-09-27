// =============================================================================
// Plik: eks_a9_ane.swift
// Opis: EKS-A9 — czy Apple Neural Engine (przez CoreML) może być trzecią
//       jednostką liczącą prefill równolegle z GPU (Metal) i CPU (Accelerate).
//       Mierzy: narzut jednego predict, czas i TFLOPS wycinka FFN 7B na ANE
//       dla wag fp16 / int4 / LUT4, przydział operacji przez MLComputePlan,
//       wejście przez MLMultiArray wobec CVPixelBuffer (IOSurface), stratę
//       GPU/ANE/CPU przy pracy równoległej oraz błąd numeryczny wobec cblas_sgemm.
// Przykład: ./run.sh a9 /sciezka/do/modeli [overhead|ffn|dist|units|plan|io|concurrent|numeric|all]
//           (modele generuje eks_a9_gen.py)
// =============================================================================

import Accelerate
import CoreML
import CoreVideo
import Foundation
import IOSurface
import Metal

// ---------------------------------------------------------------- parametry

let warmupIters = 300          // protokół N0: rozgrzewka na tym samym kształcie
let sampleRuns = 5             // pierwszy przebieg odrzucany, mediana z reszty
let predictsPerRun = 20        // jeden przebieg = średnia z tylu predict
let dModel = 4096
let nSlice = 3072
let shapesT = [256, 512, 1024]
let variants = ["fp16", "int4", "lut4"]
let concurrentWindowSec = 3.0  // jedno okno pomiaru przepustowości przy współbieżności

// ------------------------------------------------------------------ narzędzia

func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

func median(_ v: [Double]) -> Double {
    let s = v.sorted()
    if s.isEmpty { return 0 }
    return s.count % 2 == 1 ? s[s.count / 2] : (s[s.count / 2 - 1] + s[s.count / 2]) / 2
}

func iqr(_ v: [Double]) -> Double {
    let s = v.sorted()
    if s.count < 4 { return 0 }
    return s[(s.count * 3) / 4] - s[s.count / 4]
}

func thermalState() -> String {
    switch ProcessInfo.processInfo.thermalState {
    case .nominal: return "nominal"
    case .fair: return "fair"
    case .serious: return "serious"
    case .critical: return "critical"
    @unknown default: return "unknown"
    }
}

/// `pmset -g therm` — odpowiednik odczytu zegara pamięci z protokołu AMD.
func pmsetTherm() -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/bin/pmset")
    p.arguments = ["-g", "therm"]
    let pipe = Pipe()
    p.standardOutput = pipe
    try? p.run()
    p.waitUntilExit()
    let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    return out.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
        .joined(separator: "; ")
}

func logTherm(_ label: String) {
    print("stan termiczny (\(label)): \(thermalState()); pmset: \(pmsetTherm())")
}

/// Pamięć procesu: resident_size i phys_footprint z task_info. Wagi modelu ANE
/// mogą być mapowane poza procesem (aned), więc obie liczby są raportowane.
func memoryMiB() -> (rss: Double, footprint: Double) {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
    let kr = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    guard kr == KERN_SUCCESS else { return (0, 0) }
    return (Double(info.resident_size) / 1048576.0, Double(info.phys_footprint) / 1048576.0)
}

func validMark(_ spread: Double) -> String { spread <= 3.0 ? "tak" : "NIE" }

func pause(_ sec: Double) {
    Thread.sleep(forTimeInterval: sec)
}

/// Pomiar wg N0: rozgrzewka, potem `sampleRuns` przebiegów po `predictsPerRun`
/// wywołań; zwraca medianę i IQR/mediana [%] czasu jednego wywołania w µs.
func measureUs(warmup: Int = warmupIters, perRun: Int = predictsPerRun,
               _ body: () throws -> Void) rethrows -> (us: Double, spread: Double) {
    for _ in 0..<warmup { try body() }
    var samples: [Double] = []
    for _ in 0..<sampleRuns {
        let t0 = nowNs()
        for _ in 0..<perRun { try body() }
        samples.append(Double(nowNs() - t0) / 1e3 / Double(perRun))
    }
    samples.removeFirst()
    let m = median(samples)
    return (m, m > 0 ? iqr(samples) / m * 100 : 0)
}

func dirSizeMiB(_ url: URL) -> Double {
    var total: Int64 = 0
    if let e = FileManager.default.enumerator(at: url, includingPropertiesForKeys: [.fileSizeKey]) {
        for case let f as URL in e {
            total += Int64((try? f.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
        }
    }
    return Double(total) / 1048576.0
}

func loadF16(_ url: URL) -> [Float16] {
    guard let d = try? Data(contentsOf: url) else { fatalError("brak pliku \(url.path)") }
    return d.withUnsafeBytes { Array($0.bindMemory(to: Float16.self)) }
}

func loadF32(_ url: URL) -> [Float] {
    guard let d = try? Data(contentsOf: url) else { fatalError("brak pliku \(url.path)") }
    return d.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
}

// ------------------------------------------------------------------ CoreML

let args = CommandLine.arguments
let modelsDir = URL(fileURLWithPath: args.count > 1 ? args[1] : "models")
let section = args.count > 2 ? args[2] : "all"
func want(_ s: String) -> Bool { section == "all" || section == s }

func modelURL(_ name: String) -> URL { modelsDir.appendingPathComponent("\(name).mlmodelc") }

func unitsName(_ u: MLComputeUnits) -> String {
    switch u {
    case .cpuOnly: return "cpuOnly"
    case .cpuAndGPU: return "cpuAndGPU"
    case .cpuAndNeuralEngine: return "cpuAndNeuralEngine"
    case .all: return "all"
    @unknown default: return "?"
    }
}

func loadModel(_ name: String, units: MLComputeUnits) throws -> MLModel {
    let cfg = MLModelConfiguration()
    cfg.computeUnits = units
    return try MLModel(contentsOf: modelURL(name), configuration: cfg)
}

/// Wejście MLMultiArray fp16 alokowane przez CoreML, wypełnione danymi x.
func makeInput(_ model: MLModel, rows: Int, x: [Float16]) throws -> MLMultiArray {
    let desc = model.modelDescription.inputDescriptionsByName["x"]!
    let shape = desc.multiArrayConstraint!.shape
    let arr = try MLMultiArray(shape: shape, dataType: .float16)
    let p = arr.dataPointer.bindMemory(to: Float16.self, capacity: rows * dModel)
    for i in 0..<(rows * dModel) { p[i] = x[i] }
    return arr
}

func predict(_ model: MLModel, _ input: MLMultiArray, options: MLPredictionOptions? = nil) throws -> MLFeatureProvider {
    let provider = try MLDictionaryFeatureProvider(dictionary: ["x": MLFeatureValue(multiArray: input)])
    if let o = options { return try model.prediction(from: provider, options: o) }
    return try model.prediction(from: provider)
}

func flopsFFN(_ t: Int) -> Double { 2.0 * Double(t) * Double(dModel) * Double(nSlice) * 3.0 }

// ------------------------------------------------------------ MLComputePlan

/// Zlicza, na jaką jednostkę CoreML kieruje każdą operację programu.
func computePlanSummary(_ name: String, units: MLComputeUnits) -> String {
    let sem = DispatchSemaphore(value: 0)
    var result = "MLComputePlan niedostępne"
    let cfg = MLModelConfiguration()
    cfg.computeUnits = units
    Task.detached {
        defer { sem.signal() }
        do {
            let plan = try await MLComputePlan.load(contentsOf: modelURL(name), configuration: cfg)
            guard case let .program(program) = plan.modelStructure,
                  let fn = program.functions["main"] else {
                result = "struktura modelu nie jest programem MIL"
                return
            }
            var ane = 0, gpu = 0, cpu = 0, unknown = 0
            var costANE = 0.0, costAll = 0.0
            var detail: [String] = []
            for op in fn.block.operations {
                let cost = plan.estimatedCost(of: op)?.weight ?? 0
                costAll += cost
                guard let usage = plan.deviceUsage(for: op) else {
                    unknown += 1
                    detail.append("\(op.operatorName):?")
                    continue
                }
                var dev = "?"
                switch usage.preferred {
                case .neuralEngine: ane += 1; costANE += cost; dev = "ANE"
                case .gpu: gpu += 1; dev = "GPU"
                case .cpu: cpu += 1; dev = "CPU"
                @unknown default: unknown += 1
                }
                if op.operatorName != "const" { detail.append("\(op.operatorName):\(dev)") }
            }
            result = String(format: "ANE %d, GPU %d, CPU %d, brak %d ops; koszt na ANE %.0f%% (%@)",
                            ane, gpu, cpu, unknown, costAll > 0 ? costANE / costAll * 100 : 0,
                            detail.joined(separator: " "))
        } catch {
            result = "MLComputePlan błąd: \(error)"
        }
    }
    sem.wait()
    return result
}

// ------------------------------------------------------------------- Metal

/// GEMM C[M,N] = A[M,K]·B[K,N] na simdgroup_matrix (fragmenty 8×8 ładowane
/// wprost z pamięci urządzenia, 4 simdgrupy na kafel 64×64, akumulacja f32).
/// To nie jest kernel produkcyjny — ma realistycznie obciążyć GPU tym samym
/// rodzajem pracy co prefill, żeby zmierzyć, czy ANE mu przeszkadza.
let gemmSource = """
#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;

kernel void gemm_sg(device const half* A [[buffer(0)]],
                    device const half* B [[buffer(1)]],
                    device float* C [[buffer(2)]],
                    constant uint3& dims [[buffer(3)]],
                    uint2 tg [[threadgroup_position_in_grid]],
                    uint sg [[simdgroup_index_in_threadgroup]]) {
    const uint N = dims.y, K = dims.z;
    const uint row0 = tg.y * 64 + (sg / 2) * 32;
    const uint col0 = tg.x * 64 + (sg % 2) * 32;
    simdgroup_matrix<float, 8, 8> acc[4][4];
    for (uint i = 0; i < 4; ++i)
        for (uint j = 0; j < 4; ++j) acc[i][j] = simdgroup_matrix<float, 8, 8>(0);
    for (uint k = 0; k < K; k += 8) {
        simdgroup_matrix<half, 8, 8> a[4], b[4];
        for (uint i = 0; i < 4; ++i) simdgroup_load(a[i], A + (row0 + i * 8) * K + k, K);
        for (uint j = 0; j < 4; ++j) simdgroup_load(b[j], B + k * N + col0 + j * 8, N);
        for (uint i = 0; i < 4; ++i)
            for (uint j = 0; j < 4; ++j)
                simdgroup_multiply_accumulate(acc[i][j], a[i], b[j], acc[i][j]);
    }
    for (uint i = 0; i < 4; ++i)
        for (uint j = 0; j < 4; ++j)
            simdgroup_store(acc[i][j], C + (row0 + i * 8) * N + col0 + j * 8, N);
}
"""

/// Kernel z EKS-A2: łańcuchy simdgroup_multiply_accumulate na fragmentach w
/// rejestrach, zero ruchu pamięci. Kontrola negatywna dla współbieżności: jeśli
/// ANE nie spowalnia TEGO kernela, a spowalnia GEMM, to walka idzie o pasmo.
let mmaAluSource = """
#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;

kernel void mma_rate(device float* out [[buffer(0)]],
                     constant uint& iters [[buffer(1)]],
                     uint sg [[simdgroup_index_in_threadgroup]],
                     uint tgid [[threadgroup_position_in_grid]]) {
    simdgroup_matrix<half, 8, 8> a = simdgroup_matrix<half, 8, 8>(1);
    simdgroup_matrix<half, 8, 8> b = simdgroup_matrix<half, 8, 8>(1);
    simdgroup_matrix<float, 8, 8> c0 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c1 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c2 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c3 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c4 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c5 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c6 = simdgroup_matrix<float, 8, 8>(0);
    simdgroup_matrix<float, 8, 8> c7 = simdgroup_matrix<float, 8, 8>(0);
    for (uint i = 0; i < iters; ++i) {
        simdgroup_multiply_accumulate(c0, a, b, c0);
        simdgroup_multiply_accumulate(c1, a, b, c1);
        simdgroup_multiply_accumulate(c2, a, b, c2);
        simdgroup_multiply_accumulate(c3, a, b, c3);
        simdgroup_multiply_accumulate(c4, a, b, c4);
        simdgroup_multiply_accumulate(c5, a, b, c5);
        simdgroup_multiply_accumulate(c6, a, b, c6);
        simdgroup_multiply_accumulate(c7, a, b, c7);
    }
    if (tgid == 0) {
        simdgroup_store(c0, out + 0 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c1, out + 1 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c2, out + 2 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c3, out + 3 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c4, out + 4 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c5, out + 5 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c6, out + 6 * 64 + sg * 8 * 64, 8);
        simdgroup_store(c7, out + 7 * 64 + sg * 8 * 64, 8);
    }
}
"""

final class GpuAlu {
    let queue: MTLCommandQueue
    let pipe: MTLComputePipelineState
    let out: MTLBuffer
    let groups = 640, threads = 256, accs = 8
    var iters: UInt32 = 10_000
    let dispatchesPerBuffer = 4

    init?(device: MTLDevice, queue: MTLCommandQueue) {
        guard let lib = try? device.makeLibrary(source: mmaAluSource, options: nil),
              let fn = lib.makeFunction(name: "mma_rate"),
              let p = try? device.makeComputePipelineState(function: fn) else { return nil }
        self.queue = queue; pipe = p
        out = device.makeBuffer(length: 8 * 64 * 8 * 4, options: .storageModeShared)!
    }

    var flopsPerBuffer: Double {
        Double(iters) * Double(accs) * Double(threads / 32) * Double(groups) * 1024.0
            * Double(dispatchesPerBuffer)
    }

    func oneBuffer() {
        guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else { return }
        enc.setComputePipelineState(pipe)
        enc.setBuffer(out, offset: 0, index: 0)
        enc.setBytes(&iters, length: 4, index: 1)
        for _ in 0..<dispatchesPerBuffer {
            enc.dispatchThreadgroups(MTLSize(width: groups, height: 1, depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: threads, height: 1, depth: 1))
        }
        enc.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
    }
}

final class GpuGemm {
    let device: MTLDevice
    let queue: MTLCommandQueue
    let pipe: MTLComputePipelineState
    let a: MTLBuffer, b: MTLBuffer, c: MTLBuffer
    let m = 1024, n = 4096, k = 4096
    let gemmsPerBuffer = 8

    init?() {
        guard let d = MTLCreateSystemDefaultDevice(), let q = d.makeCommandQueue(),
              let lib = try? d.makeLibrary(source: gemmSource, options: nil),
              let fn = lib.makeFunction(name: "gemm_sg"),
              let p = try? d.makeComputePipelineState(function: fn) else { return nil }
        device = d; queue = q; pipe = p
        a = d.makeBuffer(length: m * k * 2, options: .storageModeShared)!
        b = d.makeBuffer(length: k * n * 2, options: .storageModeShared)!
        c = d.makeBuffer(length: m * n * 4, options: .storageModeShared)!
        let pa = a.contents().bindMemory(to: Float16.self, capacity: m * k)
        let pb = b.contents().bindMemory(to: Float16.self, capacity: k * n)
        for i in 0..<(m * k) { pa[i] = Float16(Float.random(in: -1...1)) }
        for i in 0..<(k * n) { pb[i] = Float16(Float.random(in: -0.02...0.02)) }
    }

    var flopsPerGemm: Double { 2.0 * Double(m) * Double(n) * Double(k) }

    /// Jeden bufor poleceń z `gemmsPerBuffer` mnożeniami, jak w projekcie:
    /// dyspozycje wchodzą do otwartego bufora, host czeka raz na koniec.
    func oneBuffer() {
        guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else { return }
        var dims = SIMD3<UInt32>(UInt32(m), UInt32(n), UInt32(k))
        enc.setComputePipelineState(pipe)
        enc.setBuffer(a, offset: 0, index: 0)
        enc.setBuffer(b, offset: 0, index: 1)
        enc.setBuffer(c, offset: 0, index: 2)
        enc.setBytes(&dims, length: 12, index: 3)
        for _ in 0..<gemmsPerBuffer {
            enc.dispatchThreadgroups(MTLSize(width: n / 64, height: m / 64, depth: 1),
                                     threadsPerThreadgroup: MTLSize(width: 128, height: 1, depth: 1))
        }
        enc.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
    }

    /// Kontrola poprawności kernela na jednym wierszu (błąd względny).
    func verify() -> Double {
        oneBuffer()
        let pa = a.contents().bindMemory(to: Float16.self, capacity: m * k)
        let pb = b.contents().bindMemory(to: Float16.self, capacity: k * n)
        let pc = c.contents().bindMemory(to: Float.self, capacity: m * n)
        var maxErr = 0.0
        for col in stride(from: 0, to: n, by: 97) {
            var s = 0.0
            for kk in 0..<k { s += Double(pa[5 * k + kk]) * Double(pb[kk * n + col]) }
            maxErr = max(maxErr, abs(s - Double(pc[5 * n + col])) / max(abs(s), 1e-3))
        }
        return maxErr
    }
}

// ------------------------------------------------------------------- CPU

/// Wątek C: cblas_sgemm f32 [1024 x 4096] · [4096 x 4096], jak w EKS-A7.
final class CpuGemm {
    let m = 1024, n = 4096, k = 4096
    var a: [Float], b: [Float], c: [Float]
    init() {
        a = (0..<(m * k)).map { _ in Float.random(in: -1...1) }
        b = (0..<(k * n)).map { _ in Float.random(in: -0.02...0.02) }
        c = [Float](repeating: 0, count: m * n)
    }
    var flopsPerGemm: Double { 2.0 * Double(m) * Double(n) * Double(k) }
    func one() {
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasNoTrans, Int32(m), Int32(n), Int32(k),
                    1.0, a, Int32(k), b, Int32(n), 0.0, &c, Int32(n))
    }
}

// --------------------------------------------------------------- nagłówek

print("# EKS-A9 — ANE jako trzecia jednostka prefillu (pomiar lokalny)")
print("")
let osv = ProcessInfo.processInfo.operatingSystemVersionString
print("maszyna: Apple M1, \(osv), CoreML przez Swift, katalog modeli `\(modelsDir.lastPathComponent)`")
logTherm("start")
print("")

// ====================================================== 1. narzut predict

if want("overhead") {
    print("## 1. Narzut jednego predict (modele trywialne [wiersze,dim]·[dim,dim] fp16)")
    print("")
    print("Najmniejsze kształty CoreML kieruje na CPU niezależnie od computeUnits, więc")
    print("narzut ANE czyta się z pierwszego wiersza, w którym plan mówi ANE.")
    print("")
    print("| model | computeUnits | plan (MLComputePlan) | mediana [µs] | IQR | ważny |")
    print("|---|---|---|--:|--:|---|")
    for (rows, dim) in [(1, 64), (1, 256), (8, 256), (1, 1024), (8, 1024), (64, 1024),
                        (128, 1024), (256, 1024), (512, 1024), (64, 2048), (64, 4096)] {
        let name = "trivial_\(rows)x\(dim)"
        for units in [MLComputeUnits.cpuAndNeuralEngine, .all, .cpuOnly] {
            autoreleasepool {
                do {
                    let model = try loadModel(name, units: units)
                    let input = try MLMultiArray(shape: [NSNumber(value: rows), NSNumber(value: dim)], dataType: .float16)
                    let p = input.dataPointer.bindMemory(to: Float16.self, capacity: rows * dim)
                    for i in 0..<(rows * dim) { p[i] = Float16(Float(i % 97) * 0.01) }
                    let r = try measureUs(perRun: 200) { _ = try predict(model, input) }
                    let plan = computePlanSummary(name, units: units)
                        .replacingOccurrences(of: #" \(.*\)"#, with: "", options: .regularExpression)
                    print(String(format: "| %@ | %@ | %@ | **%.1f** | %.1f%% | %@ |",
                                 name, unitsName(units), plan, r.us, r.spread, validMark(r.spread)))
                } catch {
                    print("| \(name) | \(unitsName(units)) | błąd: \(error) | | | |")
                }
            }
        }
    }
    print("")
}

// =============================================== 2. wycinek FFN na ANE

if want("ffn") {
    print("## 2. Wycinek FFN (gate/up [3072×4096], down [4096×3072]) na ANE")
    print("")
    print("FLOP na wywołanie = 2·T·4096·3072·3. Pamięć: przyrost phys_footprint procesu")
    print("po załadowaniu modelu i po pierwszym predict wobec rozmiaru mlmodelc na dysku.")
    print("")
    print("| T | wagi | units | mlmodelc [MiB] | +footprint ładowanie [MiB] | +footprint 1. predict [MiB] | mediana [µs] | IQR | ważny | TFLOPS |")
    print("|--:|---|---|--:|--:|--:|--:|--:|---|--:|")
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        for v in variants {
            for units in [MLComputeUnits.cpuAndNeuralEngine, .all] {
                let name = "ffn_T\(t)_\(v)"
                autoreleasepool {
                    do {
                        let before = memoryMiB()
                        let model = try loadModel(name, units: units)
                        let afterLoad = memoryMiB()
                        let input = try makeInput(model, rows: t, x: x)
                        _ = try predict(model, input)
                        let afterFirst = memoryMiB()
                        let r = try measureUs { _ = try predict(model, input) }
                        let tflops = flopsFFN(t) / (r.us * 1e-6) / 1e12
                        print(String(format: "| %d | %@ | %@ | %.1f | %.1f | %.1f | **%.0f** | %.1f%% | %@ | **%.2f** |",
                                     t, v, unitsName(units), dirSizeMiB(modelURL(name)),
                                     afterLoad.footprint - before.footprint,
                                     afterFirst.footprint - before.footprint,
                                     r.us, r.spread, validMark(r.spread), tflops))
                    } catch {
                        print("| \(t) | \(v) | \(unitsName(units)) | błąd: \(error) | | | | | | |")
                    }
                }
                pause(3)
            }
        }
        logTherm("po T=\(t)")
        pause(15)
    }
    print("")
}

// ============================================ 2b. rozkład pojedynczych predict

if want("dist") {
    print("## 2b. Rozkład czasu pojedynczych predict (diagnostyka IQR > 3%)")
    print("")
    print("| model | units | n | min [µs] | p10 | mediana | p90 | p99 | max | udział > 1,5·mediana |")
    print("|---|---|--:|--:|--:|--:|--:|--:|--:|--:|")
    for (name, t) in [("ffn_T1024_int4", 1024), ("ffn_T1024_fp16", 1024), ("ffn_T512_int4", 512), ("ffn_T256_int4", 256)] {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        autoreleasepool {
            do {
                let model = try loadModel(name, units: .cpuAndNeuralEngine)
                let input = try makeInput(model, rows: t, x: x)
                for _ in 0..<warmupIters { _ = try predict(model, input) }
                var times: [Double] = []
                for _ in 0..<400 {
                    let t0 = nowNs()
                    _ = try predict(model, input)
                    times.append(Double(nowNs() - t0) / 1e3)
                }
                let s = times.sorted()
                let med = median(s)
                let slow = Double(s.filter { $0 > 1.5 * med }.count) / Double(s.count) * 100
                func p(_ q: Double) -> Double { s[min(s.count - 1, Int(Double(s.count) * q))] }
                print(String(format: "| %@ | cpuAndNeuralEngine | %d | %.0f | %.0f | **%.0f** | %.0f | %.0f | %.0f | %.1f%% |",
                             name, s.count, s[0], p(0.1), med, p(0.9), p(0.99), s[s.count - 1], slow))
                // Przebieg w czasie: średnie kolejnych 50 wywołań, żeby odróżnić dryf od skoków.
                var chunks: [String] = []
                for c in stride(from: 0, to: times.count, by: 50) {
                    let slice = Array(times[c..<min(c + 50, times.count)])
                    chunks.append(String(format: "%.0f", slice.reduce(0, +) / Double(slice.count)))
                }
                print("| ↳ średnie kolejnych 50: \(chunks.joined(separator: " / ")) | | | | | | | | | |")
            } catch {
                print("| \(name) | błąd: \(error) | | | | | | | | |")
            }
        }
        pause(10)
    }
    print("")
}

// ================================= 2c. kontrola: ten sam model na innych jednostkach

if want("units") {
    print("## 2c. Kontrola: ffn_T512_int4 przez CoreML na każdej jednostce")
    print("")
    print("| units | plan | mediana [µs] | IQR | ważny | TFLOPS |")
    print("|---|---|--:|--:|---|--:|")
    let t = 512
    let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
    for units in [MLComputeUnits.cpuAndNeuralEngine, .cpuAndGPU, .cpuOnly] {
        autoreleasepool {
            do {
                let model = try loadModel("ffn_T\(t)_int4", units: units)
                let input = try makeInput(model, rows: t, x: x)
                let r = try measureUs(warmup: units == .cpuOnly ? 30 : warmupIters, perRun: units == .cpuOnly ? 5 : predictsPerRun) {
                    _ = try predict(model, input)
                }
                let plan = computePlanSummary("ffn_T\(t)_int4", units: units)
                    .replacingOccurrences(of: #" \(.*\)"#, with: "", options: .regularExpression)
                print(String(format: "| %@ | %@ | **%.0f** | %.1f%% | %@ | **%.2f** |", unitsName(units), plan,
                             r.us, r.spread, validMark(r.spread), flopsFFN(t) / (r.us * 1e-6) / 1e12))
            } catch {
                print("| \(unitsName(units)) | błąd: \(error) | | | | |")
            }
        }
        pause(10)
    }
    print("")
}

// ================================================= 3. przydział operacji

if want("plan") {
    print("## 3. Gdzie CoreML kieruje operacje (MLComputePlan)")
    print("")
    print("| model | units | przydział |")
    print("|---|---|---|")
    for t in shapesT {
        for v in variants {
            for units in [MLComputeUnits.cpuAndNeuralEngine, .all] {
                print("| ffn_T\(t)_\(v) | \(unitsName(units)) | \(computePlanSummary("ffn_T\(t)_\(v)", units: units)) |")
            }
        }
    }
    for t in shapesT {
        print("| ffn_img_T\(t)_int4 | cpuAndNeuralEngine | \(computePlanSummary("ffn_img_T\(t)_int4", units: .cpuAndNeuralEngine)) |")
    }
    print("")
    print("powermetrics (ane_power/gpu_power): wymaga sudo z hasłem na tej maszynie — pominięte.")
    print("")
}

// ============================================ 4. wejście/wyjście bez kopii

if want("io") {
    print("## 4. Wejście/wyjście: MLMultiArray wobec CVPixelBuffer (IOSurface), int4")
    print("")
    print("Graf rank-4 [1,1,T,4096] jest ten sam dla obu; różni się wyłącznie typ")
    print("wejścia/wyjścia. Wariant (a2) to MLMultiArray na własnym buforze wyrównanym")
    print("do strony z `outputBackings`, (b) to obraz OneComponent16Half w obie strony.")
    print("")
    print("| T | wariant | mediana [µs] | IQR | ważny | wobec (a) |")
    print("|--:|---|--:|--:|---|--:|")
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        var baseUs = 0.0
        autoreleasepool {
            do {
                // (a) MLMultiArray alokowany przez CoreML, wyjście alokowane przez CoreML.
                let model = try loadModel("ffn_r4_T\(t)_int4", units: .cpuAndNeuralEngine)
                let input = try makeInput(model, rows: t, x: x)
                let ra = try measureUs { _ = try predict(model, input) }
                baseUs = ra.us
                print(String(format: "| %d | (a) MLMultiArray CoreML | **%.0f** | %.1f%% | %@ | — |",
                             t, ra.us, ra.spread, validMark(ra.spread)))

                // (a2) bufory wyrównane do strony, wyjście przez outputBackings.
                let bytes = t * dModel * 2
                var pin: UnsafeMutableRawPointer? = nil
                var pout: UnsafeMutableRawPointer? = nil
                posix_memalign(&pin, 16384, bytes)
                posix_memalign(&pout, 16384, bytes)
                let pi = pin!.bindMemory(to: Float16.self, capacity: t * dModel)
                for i in 0..<(t * dModel) { pi[i] = x[i] }
                let shape: [NSNumber] = [1, 1, NSNumber(value: t), NSNumber(value: dModel)]
                let strides: [NSNumber] = [NSNumber(value: t * dModel), NSNumber(value: t * dModel),
                                           NSNumber(value: dModel), 1]
                let inArr = try MLMultiArray(dataPointer: pin!, shape: shape, dataType: .float16,
                                             strides: strides, deallocator: nil)
                let outArr = try MLMultiArray(dataPointer: pout!, shape: shape, dataType: .float16,
                                              strides: strides, deallocator: nil)
                let opts = MLPredictionOptions()
                opts.outputBackings = ["y": outArr]
                let ra2 = try measureUs { _ = try predict(model, inArr, options: opts) }
                print(String(format: "| %d | (a2) MLMultiArray page-aligned + outputBackings | **%.0f** | %.1f%% | %@ | %+.1f%% |",
                             t, ra2.us, ra2.spread, validMark(ra2.spread), (ra2.us / baseUs - 1) * 100))

                // (b) CVPixelBuffer OneComponent16Half na IOSurface, w obie strony.
                let imgModel = try loadModel("ffn_img_T\(t)_int4", units: .cpuAndNeuralEngine)
                let attrs: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary]
                var pbIn: CVPixelBuffer? = nil
                var pbOut: CVPixelBuffer? = nil
                let s1 = CVPixelBufferCreate(kCFAllocatorDefault, dModel, t, kCVPixelFormatType_OneComponent16Half,
                                             attrs as CFDictionary, &pbIn)
                let s2 = CVPixelBufferCreate(kCFAllocatorDefault, dModel, t, kCVPixelFormatType_OneComponent16Half,
                                             attrs as CFDictionary, &pbOut)
                guard s1 == kCVReturnSuccess, s2 == kCVReturnSuccess, let pbi = pbIn, let pbo = pbOut else {
                    print("| \(t) | (b) CVPixelBuffer | utworzenie nieudane (\(s1), \(s2)) | | | |")
                    return
                }
                let hasSurface = CVPixelBufferGetIOSurface(pbi) != nil
                CVPixelBufferLockBaseAddress(pbi, [])
                let stride = CVPixelBufferGetBytesPerRow(pbi)
                let base = CVPixelBufferGetBaseAddress(pbi)!
                for r in 0..<t {
                    let row = (base + r * stride).bindMemory(to: Float16.self, capacity: dModel)
                    for cix in 0..<dModel { row[cix] = x[r * dModel + cix] }
                }
                CVPixelBufferUnlockBaseAddress(pbi, [])
                let provider = try MLDictionaryFeatureProvider(dictionary: ["x": MLFeatureValue(pixelBuffer: pbi)])
                let optsB = MLPredictionOptions()
                optsB.outputBackings = ["y": pbo]
                let rb = try measureUs { _ = try imgModel.prediction(from: provider, options: optsB) }
                print(String(format: "| %d | (b) CVPixelBuffer IOSurface%@ we/wy | **%.0f** | %.1f%% | %@ | %+.1f%% |",
                             t, hasSurface ? "" : " (BEZ IOSurface!)", rb.us, rb.spread, validMark(rb.spread),
                             (rb.us / baseUs - 1) * 100))
                // Kontrola: wyjście z obrazu równe wyjściu z tablicy.
                let outA = try predict(model, input).featureValue(for: "y")!.multiArrayValue!
                CVPixelBufferLockBaseAddress(pbo, .readOnly)
                let ob = CVPixelBufferGetBaseAddress(pbo)!
                let ostride = CVPixelBufferGetBytesPerRow(pbo)
                var maxDiff: Float = 0
                let pa = outA.dataPointer.bindMemory(to: Float16.self, capacity: t * dModel)
                for r in 0..<t {
                    let row = (ob + r * ostride).bindMemory(to: Float16.self, capacity: dModel)
                    for cix in 0..<dModel {
                        maxDiff = max(maxDiff, abs(Float(row[cix]) - Float(pa[r * dModel + cix])))
                    }
                }
                CVPixelBufferUnlockBaseAddress(pbo, .readOnly)
                print(String(format: "| %d | kontrola: max |y_obraz − y_tablica| | %.3g | | | |", t, maxDiff))
            } catch {
                print("| \(t) | błąd: \(error) | | | | |")
            }
        }
        pause(10)
    }
    print("")
}

// ======================================================= 5. współbieżność

if want("concurrent") {
    print("## 5. Współbieżność: ANE (T=1024, int4) + GPU (simdgroup_matrix) + CPU (cblas_sgemm)")
    print("")
    logTherm("przed współbieżnością")
    guard let gpu = GpuGemm() else { fatalError("brak Metala") }
    let gpuErr = gpu.verify()
    print(String(format: "kontrola kernela GEMM: max błąd względny %.2e (GEMM [%d×%d]·[%d×%d] f16, akumulacja f32, %d dyspozycji na bufor poleceń)",
                 gpuErr, gpu.m, gpu.k, gpu.k, gpu.n, gpu.gemmsPerBuffer))
    guard let alu = GpuAlu(device: gpu.device, queue: gpu.queue) else { fatalError("brak kernela ALU") }
    let cpu = CpuGemm()
    let x = loadF16(modelsDir.appendingPathComponent("x_T1024.f16.bin"))
    let aneModel = try! loadModel("ffn_T1024_int4", units: .cpuAndNeuralEngine)
    let aneInput = try! makeInput(aneModel, rows: 1024, x: x)

    // Rozgrzewka każdej jednostki osobno (protokół N0).
    for _ in 0..<warmupIters { _ = try! predict(aneModel, aneInput) }
    for _ in 0..<40 { gpu.oneBuffer() }
    for _ in 0..<6 { alu.oneBuffer() }
    for _ in 0..<30 { cpu.one() }

    struct Worker {
        let name: String
        let flopsPerOp: Double
        let op: () -> Void
    }
    let wANE = Worker(name: "ANE", flopsPerOp: flopsFFN(1024)) { _ = try! predict(aneModel, aneInput) }
    let wGPU = Worker(name: "GPU", flopsPerOp: gpu.flopsPerGemm * Double(gpu.gemmsPerBuffer)) { gpu.oneBuffer() }
    let wCPU = Worker(name: "CPU", flopsPerOp: cpu.flopsPerGemm) { cpu.one() }
    let wALU = Worker(name: "GPU-ALU", flopsPerOp: alu.flopsPerBuffer) { alu.oneBuffer() }

    /// Uruchamia zestaw wątków przez `concurrentWindowSec`; zwraca TFLOPS każdego.
    func window(_ workers: [Worker]) -> [Double] {
        let deadline = nowNs() + UInt64(concurrentWindowSec * 1e9)
        var results = [Double](repeating: 0, count: workers.count)
        let group = DispatchGroup()
        let lock = NSLock()
        for (i, w) in workers.enumerated() {
            group.enter()
            Thread.detachNewThread {
                var ops = 0
                let t0 = nowNs()
                while nowNs() < deadline { w.op(); ops += 1 }
                let secs = Double(nowNs() - t0) / 1e9
                lock.lock(); results[i] = Double(ops) * w.flopsPerOp / secs / 1e12; lock.unlock()
                group.leave()
            }
        }
        group.wait()
        return results
    }

    /// 5 okien, pierwsze odrzucone; mediana i IQR TFLOPS każdego wątku.
    func series(_ workers: [Worker]) -> [(med: Double, spread: Double)] {
        var samples = [[Double]](repeating: [], count: workers.count)
        for _ in 0..<sampleRuns {
            let r = window(workers)
            for i in 0..<workers.count { samples[i].append(r[i]) }
            pause(1)
        }
        return samples.map { s in
            var v = s; v.removeFirst()
            let m = median(v)
            return (m, m > 0 ? iqr(v) / m * 100 : 0)
        }
    }

    let conditions: [(String, [Worker])] = [
        ("GPU sam", [wGPU]),
        ("ANE sam", [wANE]),
        ("CPU sam", [wCPU]),
        ("GPU + ANE", [wGPU, wANE]),
        ("GPU + CPU", [wGPU, wCPU]),
        ("ANE + CPU", [wANE, wCPU]),
        ("GPU + ANE + CPU", [wGPU, wANE, wCPU]),
        ("GPU-ALU sam (EKS-A2, bez pamięci)", [wALU]),
        ("GPU-ALU + ANE", [wALU, wANE]),
    ]
    var solo: [String: Double] = [:]
    print("")
    print("| warunek | wątek | TFLOPS mediana | IQR | ważny | strata wobec „sam” |")
    print("|---|---|--:|--:|---|--:|")
    for (label, workers) in conditions {
        let res = series(workers)
        for (i, w) in workers.enumerated() {
            let r = res[i]
            if workers.count == 1 { solo[w.name] = r.med }
            let loss = solo[w.name].map { (1 - r.med / $0) * 100 }
            print(String(format: "| %@ | %@ | **%.3f** | %.1f%% | %@ | %@ |",
                         label, w.name, r.med, r.spread, validMark(r.spread),
                         workers.count == 1 ? "—" : String(format: "%.1f%%", loss ?? 0)))
        }
        logTherm("po „\(label)”")
        pause(20)
    }
    print("")
}

// ============================================================ 6. numeryka

if want("numeric") {
    print("## 6. Numeryka (T=512): wyjście ANE wobec referencji fp32 (cblas_sgemm)")
    print("")
    print("Referencja liczy te same wagi, które zapisał coremltools (zdekwantyzowane do")
    print("f32), więc błąd mierzy arytmetykę ANE, nie kwantyzację. Osobno podana jest")
    print("kwantyzacja sama: referencja wariantu wobec referencji fp16.")
    print("")
    let t = 512
    let x16 = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
    let x32 = x16.map { Float($0) }

    func reference(_ v: String) -> [Float] {
        let wg = loadF32(modelsDir.appendingPathComponent("w_gate_\(v).f32.bin"))
        let wu = loadF32(modelsDir.appendingPathComponent("w_up_\(v).f32.bin"))
        let wd = loadF32(modelsDir.appendingPathComponent("w_down_\(v).f32.bin"))
        var gate = [Float](repeating: 0, count: t * nSlice)
        var up = [Float](repeating: 0, count: t * nSlice)
        var out = [Float](repeating: 0, count: t * dModel)
        // gate = x · Wg^T  (Wg [nSlice × dModel] w układzie wierszowym)
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, Int32(t), Int32(nSlice), Int32(dModel),
                    1, x32, Int32(dModel), wg, Int32(dModel), 0, &gate, Int32(nSlice))
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, Int32(t), Int32(nSlice), Int32(dModel),
                    1, x32, Int32(dModel), wu, Int32(dModel), 0, &up, Int32(nSlice))
        for i in 0..<(t * nSlice) {
            let g = gate[i]
            gate[i] = g / (1 + expf(-g)) * up[i]
        }
        // out = h · Wd^T  (Wd [dModel × nSlice])
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, Int32(t), Int32(dModel), Int32(nSlice),
                    1, gate, Int32(nSlice), wd, Int32(nSlice), 0, &out, Int32(dModel))
        return out
    }

    func errors(_ y: [Float], _ ref: [Float]) -> (relL2: Double, maxAbs: Double, refMax: Double) {
        var num = 0.0, den = 0.0, mx = 0.0, rm = 0.0
        for i in 0..<ref.count {
            let d = Double(y[i]) - Double(ref[i])
            num += d * d; den += Double(ref[i]) * Double(ref[i])
            mx = max(mx, abs(d)); rm = max(rm, abs(Double(ref[i])))
        }
        return (sqrt(num / den), mx, rm)
    }

    let refFp16 = reference("fp16")
    print("| wagi | wykonawca | względna L2 | max |Δ| | max |ref| |")
    print("|---|---|--:|--:|--:|")
    for v in variants {
        let ref = v == "fp16" ? refFp16 : reference(v)
        if v != "fp16" {
            let q = errors(ref, refFp16)
            print(String(format: "| %@ | kwantyzacja sama (CPU f32 wobec CPU f32 fp16) | %.3e | %.3e | %.3e |", v, q.relL2, q.maxAbs, q.refMax))
        }
        for units in [MLComputeUnits.cpuAndNeuralEngine, .all, .cpuOnly] {
            autoreleasepool {
                do {
                    let model = try loadModel("ffn_T\(t)_\(v)", units: units)
                    let input = try makeInput(model, rows: t, x: x16)
                    let out = try predict(model, input).featureValue(for: "y")!.multiArrayValue!
                    let p = out.dataPointer.bindMemory(to: Float16.self, capacity: t * dModel)
                    var y = [Float](repeating: 0, count: t * dModel)
                    for i in 0..<(t * dModel) { y[i] = Float(p[i]) }
                    let e = errors(y, ref)
                    print(String(format: "| %@ | CoreML %@ | **%.3e** | %.3e | %.3e |", v, unitsName(units), e.relL2, e.maxAbs, e.refMax))
                } catch {
                    print("| \(v) | CoreML \(unitsName(units)) | błąd: \(error) | | |")
                }
            }
        }
    }
    print("")
}

logTherm("koniec")

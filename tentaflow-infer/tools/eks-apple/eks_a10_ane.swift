// =============================================================================
// Plik: eks_a10_ane.swift
// Opis: EKS-A10 faza 0 — trzy sondy rozstrzygające przed implementacją ANE
//       w Rust: (0.1) kodowanie 4-bit blokowe grupa 64 (jak MLX affine) przez
//       constexpr_blockwise_shift_scale wobec int4 per-channel — plan i TFLOPS
//       przeplatane; (0.2) jeden mlpackage multifunction T256/T512/T1024 na
//       wspólnych wagach — rozmiar, ładowanie zimne/ciepłe, pamięć, TFLOPS
//       funkcji wobec osobnych modeli; (0.3) wyjście z krokiem (outputBackings
//       na buforze szerszym niż wynik) — czy CoreML pisze in-place, czy kopiuje.
// Przykład: ./run.sh a10 /sciezka/do/modeli_a10 [plan|ffn|numeric|multi|multimem|multimem-rev|strided|all]
//           (modele generuje eks_a9_gen.py --a10)
// =============================================================================

import Accelerate
import CoreML
import Foundation

// ---------------------------------------------------------------- parametry

let warmupIters = 300          // protokół N0: rozgrzewka na tym samym kształcie
let interleavedRounds = 9      // rund przeplatanych (A,B,C,…)×9, pierwsza odrzucona → 8 próbek
let predictsPerRun = 20        // jedna runda jednego wariantu = średnia z tylu predict
let dModel = 4096
let nSlice = 3072
let shapesT = [256, 512, 1024]
let encodings = ["int4", "blockwise", "blockwise_zp", "affine16"]
let multiVariant = ProcessInfo.processInfo.environment["A10_MULTI_VARIANT"] ?? "blockwise"
let stridedVariant = ProcessInfo.processInfo.environment["A10_STRIDED_VARIANT"] ?? "blockwise"
let rowsTotal = 11264          // szerokość bufora docelowego (pełny wymiar inter 7B)

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

func shell(_ path: String, _ args: [String]) -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    p.arguments = args
    let pipe = Pipe()
    p.standardOutput = pipe
    try? p.run()
    p.waitUntilExit()
    return String(data: pipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
}

func logTherm(_ label: String) {
    let pm = shell("/usr/bin/pmset", ["-g", "therm"]).split(separator: "\n")
        .map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: "; ")
    print("stan termiczny (\(label)): \(thermalState()); pmset: \(pm)")
}

/// Pamięć procesu: phys_footprint z task_info (wagi ANE mogą żyć poza procesem).
func footprintMiB() -> Double {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
    let kr = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    guard kr == KERN_SUCCESS else { return 0 }
    return Double(info.phys_footprint) / 1048576.0
}

/// Pamięć systemowa: vm_stat (strony 16 KiB) — wolne, wired, kompresor; oraz RSS aned.
struct SysMem {
    var freeMiB = 0.0, wiredMiB = 0.0, compressorMiB = 0.0, anedRssMiB = 0.0, procMiB = 0.0
    static func now() -> SysMem {
        var m = SysMem()
        let out = shell("/usr/bin/vm_stat", [])
        func pages(_ key: String) -> Double {
            for line in out.split(separator: "\n") where line.hasPrefix(key) {
                let num = line.split(separator: ":").last?.trimmingCharacters(in: CharacterSet(charactersIn: " ."))
                return Double(num ?? "0") ?? 0
            }
            return 0
        }
        m.freeMiB = pages("Pages free") * 16384 / 1048576
        m.wiredMiB = pages("Pages wired down") * 16384 / 1048576
        m.compressorMiB = pages("Pages occupied by compressor") * 16384 / 1048576
        let pid = shell("/usr/bin/pgrep", ["-x", "aned"]).trimmingCharacters(in: .whitespacesAndNewlines)
        if !pid.isEmpty {
            let rss = shell("/bin/ps", ["-o", "rss=", "-p", pid]).trimmingCharacters(in: .whitespacesAndNewlines)
            m.anedRssMiB = (Double(rss) ?? 0) / 1024
        }
        m.procMiB = footprintMiB()
        return m
    }
    func delta(_ b: SysMem) -> String {
        String(format: "proces %+.1f, aned RSS %+.1f, wired %+.1f, wolne %+.1f, kompresor %+.1f",
               procMiB - b.procMiB, anedRssMiB - b.anedRssMiB, wiredMiB - b.wiredMiB,
               freeMiB - b.freeMiB, compressorMiB - b.compressorMiB)
    }
}

func validMark(_ spread: Double) -> String { spread <= 3.0 ? "tak" : "NIE" }

func pause(_ sec: Double) { Thread.sleep(forTimeInterval: sec) }

/// Pomiar PRZEPLATANY: każdy wariant rozgrzany osobno, potem `rounds` rund, w każdej
/// rundzie każdy wariant po kolei wykonuje `perRun` wywołań. Pierwsza runda odrzucona.
/// Zwraca medianę i IQR/mediana [%] czasu jednego wywołania [µs] dla każdego wariantu.
/// Przeplatanie zdejmuje z porównania dryf maszyny (inne procesy, DVFS, temperatura).
func measureInterleaved(warmup: Int = warmupIters, perRun: Int = predictsPerRun,
                        rounds: Int = interleavedRounds,
                        _ bodies: [() throws -> Void]) throws -> [(us: Double, spread: Double)] {
    for b in bodies { for _ in 0..<warmup { try b() } }
    var samples = [[Double]](repeating: [], count: bodies.count)
    for _ in 0..<rounds {
        for (i, b) in bodies.enumerated() {
            let t0 = nowNs()
            for _ in 0..<perRun { try b() }
            samples[i].append(Double(nowNs() - t0) / 1e3 / Double(perRun))
        }
    }
    return samples.map { s in
        var v = s
        v.removeFirst()
        let m = median(v)
        return (m, m > 0 ? iqr(v) / m * 100 : 0)
    }
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

/// Katalog cache skompilowanych programów ANE tego procesu (~/Library/Caches/<nazwa binarki>/…).
let e5CacheURL = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent("Library/Caches/\(ProcessInfo.processInfo.processName)/com.apple.e5rt.e5bundlecache")

// ------------------------------------------------------------------ CoreML

let args = CommandLine.arguments
let modelsDir = URL(fileURLWithPath: args.count > 1 ? args[1] : "models_a10")
let section = args.count > 2 ? args[2] : "all"
func want(_ s: String) -> Bool { section == "all" || section == s }

func modelURL(_ name: String) -> URL { modelsDir.appendingPathComponent("\(name).mlmodelc") }

func config(_ units: MLComputeUnits = .cpuAndNeuralEngine, function: String? = nil) -> MLModelConfiguration {
    let cfg = MLModelConfiguration()
    cfg.computeUnits = units
    if let f = function { cfg.functionName = f }
    return cfg
}

func loadModel(_ name: String, function: String? = nil) throws -> MLModel {
    try MLModel(contentsOf: modelURL(name), configuration: config(function: function))
}

/// Wejście na własnym buforze wyrównanym do strony (jak wariant (a2) z A9).
func makeInput(rows: Int, x: [Float16]) throws -> MLMultiArray {
    var p: UnsafeMutableRawPointer? = nil
    posix_memalign(&p, 16384, rows * dModel * 2)
    let pi = p!.bindMemory(to: Float16.self, capacity: rows * dModel)
    for i in 0..<(rows * dModel) { pi[i] = x[i] }
    return try MLMultiArray(dataPointer: p!, shape: [NSNumber(value: rows), NSNumber(value: dModel)],
                            dataType: .float16, strides: [NSNumber(value: dModel), 1], deallocator: nil)
}

func predict(_ model: MLModel, _ input: MLMultiArray, options: MLPredictionOptions? = nil) throws -> MLFeatureProvider {
    let provider = try MLDictionaryFeatureProvider(dictionary: ["x": MLFeatureValue(multiArray: input)])
    if let o = options { return try model.prediction(from: provider, options: o) }
    return try model.prediction(from: provider)
}

func flopsFFN(_ t: Int) -> Double { 2.0 * Double(t) * Double(dModel) * Double(nSlice) * 3.0 }

func tflops(_ t: Int, _ us: Double) -> Double { flopsFFN(t) / (us * 1e-6) / 1e12 }

// ------------------------------------------------------------ MLComputePlan

/// Zlicza, na jaką jednostkę CoreML kieruje każdą operację funkcji programu.
func computePlanSummary(_ name: String, function: String = "main") -> String {
    let sem = DispatchSemaphore(value: 0)
    var result = "MLComputePlan niedostępne"
    let cfg = config(function: function == "main" ? nil : function)
    Task.detached {
        defer { sem.signal() }
        do {
            let plan = try await MLComputePlan.load(contentsOf: modelURL(name), configuration: cfg)
            guard case let .program(program) = plan.modelStructure,
                  let fn = program.functions[function] else {
                result = "brak funkcji \(function) w programie MIL"
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
                    if op.operatorName != "const" { detail.append("\(op.operatorName):-") }
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
            result = String(format: "ANE %d, GPU %d, CPU %d, brak %d; koszt na ANE %.0f%% (%@)",
                            ane, gpu, cpu, unknown, costAll > 0 ? costANE / costAll * 100 : 0,
                            detail.joined(separator: " "))
        } catch {
            result = "MLComputePlan błąd: \(error)"
        }
    }
    sem.wait()
    return result
}

// --------------------------------------------------------------- nagłówek

print("# EKS-A10 faza 0 — blockwise / multifunction / wyjście z krokiem (pomiar lokalny)")
print("")
let osv = ProcessInfo.processInfo.operatingSystemVersionString
print("maszyna: Apple M1, \(osv), CoreML przez Swift, katalog modeli `\(modelsDir.lastPathComponent)`, sekcja `\(section)`")
logTherm("start")
print("")

// ============================================================ 0.1 plan

if want("plan") {
    print("## 0.1a Gdzie CoreML kieruje operacje (MLComputePlan, cpuAndNeuralEngine)")
    print("")
    print("| model | funkcja | przydział |")
    print("|---|---|---|")
    for t in shapesT {
        for enc in encodings {
            print("| ffn_T\(t)_\(enc) | main | \(computePlanSummary("ffn_T\(t)_\(enc)")) |")
        }
    }
    for t in shapesT {
        print("| ffn_multi_\(multiVariant) | T\(t) | \(computePlanSummary("ffn_multi_\(multiVariant)", function: "T\(t)")) |")
    }
    print("")
}

// ============================================================ 0.1 sondy kodowania

if want("probe") {
    print("## 0.1d Sondy kodowania grupowego (T=512): plan i czas przeplatany (int4 per-channel = 100%)")
    print("")
    print("Modele z `probe.txt` (buduje probe_gen.py). Czas mierzony tylko dla modeli w 100% na ANE;")
    print("model z operacją na CPU dostaje jeden przebieg orientacyjny (bez protokołu N0).")
    print("")
    print("| model | przydział (MLComputePlan) | mlmodelc [MiB] | mediana [µs] | IQR | ważny | TFLOPS | wobec int4 |")
    print("|---|---|--:|--:|--:|---|--:|--:|")
    let t = 512
    let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
    let listed = (try? String(contentsOf: modelsDir.appendingPathComponent("probe.txt"), encoding: .utf8)) ?? ""
    let names = ["ffn_T512_int4"] + listed.split(separator: "\n").map(String.init).filter { !$0.isEmpty }
    var plans: [String: String] = [:]
    var onANE: [String] = []
    for n in names {
        let p = computePlanSummary(n)
        plans[n] = p
        if p.contains("GPU 0, CPU 0") { onANE.append(n) }
    }
    autoreleasepool {
        do {
            let input = try makeInput(rows: t, x: x)
            var models: [MLModel] = []
            for n in onANE { models.append(try loadModel(n)) }
            let res = try measureInterleaved(models.map { m in { _ = try predict(m, input) } })
            for n in names {
                let short = plans[n]!.replacingOccurrences(of: #" \(.*\)"#, with: "", options: .regularExpression)
                if let i = onANE.firstIndex(of: n) {
                    let r = res[i]
                    print(String(format: "| %@ | %@ | %.1f | **%.0f** | %.1f%% | %@ | **%.2f** | %.1f%% |", n, short,
                                 dirSizeMiB(modelURL(n)), r.us, r.spread, validMark(r.spread), tflops(t, r.us), res[0].us / r.us * 100))
                } else {
                    let m = try loadModel(n)
                    for _ in 0..<3 { _ = try predict(m, input) }
                    let t0 = nowNs()
                    for _ in 0..<5 { _ = try predict(m, input) }
                    let us = Double(nowNs() - t0) / 1e3 / 5
                    print(String(format: "| %@ | %@ | %.1f | %.0f (orientacyjnie) | — | — | %.2f | %.1f%% |", n, short,
                                 dirSizeMiB(modelURL(n)), us, tflops(t, us), res[0].us / us * 100))
                }
            }
        } catch {
            print("| błąd: \(error) | | | | | | | |")
        }
    }
    print("")
}

// ============================================================ 0.1 ffn

if want("ffn") {
    print("## 0.1b Wycinek FFN: kodowanie wag, pomiar przeplatany (int4 per-channel = 100%)")
    print("")
    print("Warianty załadowane naraz, rozgrzewka \(warmupIters) każdy, potem \(interleavedRounds) rund")
    print("(w rundzie każdy wariant po \(predictsPerRun) predict), pierwsza runda odrzucona, mediana i IQR z \(interleavedRounds - 1).")
    print("")
    print("| T | kodowanie | mlmodelc [MiB] | mediana [µs] | IQR | ważny | TFLOPS | wobec int4 |")
    print("|--:|---|--:|--:|--:|---|--:|--:|")
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        autoreleasepool {
            do {
                var models: [MLModel] = []
                for enc in encodings { models.append(try loadModel("ffn_T\(t)_\(enc)")) }
                let input = try makeInput(rows: t, x: x)
                let res = try measureInterleaved(models.map { m in { _ = try predict(m, input) } })
                for (i, enc) in encodings.enumerated() {
                    let r = res[i]
                    print(String(format: "| %d | %@ | %.1f | **%.0f** | %.1f%% | %@ | **%.2f** | %.1f%% |",
                                 t, enc, dirSizeMiB(modelURL("ffn_T\(t)_\(enc)")), r.us, r.spread,
                                 validMark(r.spread), tflops(t, r.us), res[0].us / r.us * 100))
                }
            } catch {
                print("| \(t) | błąd: \(error) | | | | | | |")
            }
        }
        logTherm("po T=\(t)")
        pause(10)
    }
    print("")
}

// ============================================================ 0.1 numeryka

if want("numeric") {
    print("## 0.1c Numeryka (T=512): wyjście ANE wobec referencji fp32 (cblas_sgemm)")
    print("")
    print("Referencja „coremltools” liczy wagi zdekwantyzowane przez decompress_weights")
    print("(scale·(q−offset_f16)); referencja „MLX” liczy q·scale+bias na tych samych q/scale/bias.")
    print("")
    let t = 512
    let x16 = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
    let x32 = x16.map { Float($0) }

    func reference(_ suffix: String) -> [Float] {
        let wg = loadF32(modelsDir.appendingPathComponent("w_gate_\(suffix).f32.bin"))
        let wu = loadF32(modelsDir.appendingPathComponent("w_up_\(suffix).f32.bin"))
        let wd = loadF32(modelsDir.appendingPathComponent("w_down_\(suffix).f32.bin"))
        var gate = [Float](repeating: 0, count: t * nSlice)
        var up = [Float](repeating: 0, count: t * nSlice)
        var out = [Float](repeating: 0, count: t * dModel)
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, Int32(t), Int32(nSlice), Int32(dModel),
                    1, x32, Int32(dModel), wg, Int32(dModel), 0, &gate, Int32(nSlice))
        cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, Int32(t), Int32(nSlice), Int32(dModel),
                    1, x32, Int32(dModel), wu, Int32(dModel), 0, &up, Int32(nSlice))
        for i in 0..<(t * nSlice) {
            let g = gate[i]
            gate[i] = g / (1 + expf(-g)) * up[i]
        }
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

    func aneOutput(_ name: String) throws -> [Float] {
        let model = try loadModel(name)
        let input = try makeInput(rows: t, x: x16)
        let out = try predict(model, input).featureValue(for: "y")!.multiArrayValue!
        let p = out.dataPointer.bindMemory(to: Float16.self, capacity: t * dModel)
        return (0..<(t * dModel)).map { Float(p[$0]) }
    }

    print("| wagi | wykonawca | referencja | względna L2 | max |Δ| | max |ref| |")
    print("|---|---|---|--:|--:|--:|")
    do {
        let refBlock = reference("blockwise")
        let refMlx = reference("mlx")
        let refInt4 = reference("int4")
        let q = errors(refBlock, refMlx)
        print(String(format: "| blockwise | CPU f32 (coremltools) | CPU f32 (MLX) | %.3e | %.3e | %.3e |", q.relL2, q.maxAbs, q.refMax))
        let yb = try aneOutput("ffn_T\(t)_blockwise")
        var e = errors(yb, refBlock)
        print(String(format: "| blockwise | ANE | coremltools | **%.3e** | %.3e | %.3e |", e.relL2, e.maxAbs, e.refMax))
        e = errors(yb, refMlx)
        print(String(format: "| blockwise | ANE | MLX | **%.3e** | %.3e | %.3e |", e.relL2, e.maxAbs, e.refMax))
        let yz = try aneOutput("ffn_T\(t)_blockwise_zp")
        e = errors(yz, refMlx)
        print(String(format: "| blockwise_zp | ANE | MLX | %.3e | %.3e | %.3e |", e.relL2, e.maxAbs, e.refMax))
        let yi = try aneOutput("ffn_T\(t)_int4")
        e = errors(yi, refInt4)
        print(String(format: "| int4 per-channel | ANE | coremltools | %.3e | %.3e | %.3e |", e.relL2, e.maxAbs, e.refMax))
        // Re-kodowanie wag MLX (grupa 64, zdekwantyzowane) do formatu, który ANE przyjmuje:
        // int8 / int4 per-channel symetryczne. Błąd wobec referencji MLX = koszt re-kodowania + ANE.
        for enc in ["int8pc_mlx", "int4pc_mlx"] {
            let refEnc = reference(enc)
            let q = errors(refEnc, refMlx)
            print(String(format: "| %@ | CPU f32 (re-kodowanie samo) | MLX | %.3e | %.3e | %.3e |", enc, q.relL2, q.maxAbs, q.refMax))
            let y = try aneOutput("probe_\(enc)")
            e = errors(y, refEnc)
            print(String(format: "| %@ | ANE | coremltools (%@) | %.3e | %.3e | %.3e |", enc, enc, e.relL2, e.maxAbs, e.refMax))
            e = errors(y, refMlx)
            print(String(format: "| %@ | ANE | MLX | **%.3e** | %.3e | %.3e |", enc, e.relL2, e.maxAbs, e.refMax))
        }
        let refF16 = reference("fp16")
        let q16 = errors(refMlx, refF16)
        print(String(format: "| MLX grupa 64 | CPU f32 (kwantyzacja sama) | fp16 | %.3e | %.3e | %.3e |", q16.relL2, q16.maxAbs, q16.refMax))
        for tt in shapesT {
            // Funkcja multifunction wobec osobnego modelu: bit w bit?
            let xx = loadF16(modelsDir.appendingPathComponent("x_T\(tt).f16.bin"))
            let inp = try makeInput(rows: tt, x: xx)
            let a = try predict(try loadModel("ffn_T\(tt)_\(multiVariant)"), inp).featureValue(for: "y")!.multiArrayValue!
            let b = try predict(try loadModel("ffn_multi_\(multiVariant)", function: "T\(tt)"), inp).featureValue(for: "y")!.multiArrayValue!
            let pa = a.dataPointer.bindMemory(to: UInt16.self, capacity: tt * dModel)
            let pb = b.dataPointer.bindMemory(to: UInt16.self, capacity: tt * dModel)
            var diff = 0
            for i in 0..<(tt * dModel) where pa[i] != pb[i] { diff += 1 }
            print("| \(multiVariant) T=\(tt) | ANE multifunction | ANE osobny model | bajty różne: \(diff) z \(tt * dModel) | | |")
        }
    } catch {
        print("| błąd: \(error) | | | | | |")
    }
    print("")
}

// ============================================================ 0.2 multifunction

if want("multi") {
    print("## 0.2 Multifunction: rozmiar, ładowanie, TFLOPS funkcji wobec osobnych modeli (\(multiVariant))")
    print("")
    let multiName = "ffn_multi_\(multiVariant)"
    var sumPkg = 0.0, sumC = 0.0
    for t in shapesT {
        sumPkg += dirSizeMiB(modelsDir.appendingPathComponent("ffn_T\(t)_\(multiVariant).mlpackage"))
        sumC += dirSizeMiB(modelURL("ffn_T\(t)_\(multiVariant)"))
    }
    print("| | mlpackage [MiB] | mlmodelc [MiB] |")
    print("|---|--:|--:|")
    print(String(format: "| suma trzech osobnych (T256+T512+T1024) | %.2f | %.2f |", sumPkg, sumC))
    print(String(format: "| multifunction (3 funkcje) | %.2f | %.2f |",
                 dirSizeMiB(modelsDir.appendingPathComponent(multiName + ".mlpackage")), dirSizeMiB(modelURL(multiName))))
    print("")
    print("Ładowanie: `MLModel(contentsOf:configuration:)` z `functionName`. Cache ANE tego procesu:")
    print("`\(e5CacheURL.path)` — jeśli rośnie po ładowaniu, to ładowanie było zimne (kompilacja w aned).")
    print("")
    print("| model | funkcja | ładowanie [ms] | cache ANE przed → po [MiB] | 1. predict [µs] | ponowne ładowanie w procesie [ms] |")
    print("|---|---|--:|---|--:|--:|")
    func loadTimed(_ name: String, function: String?, t: Int, x: [Float16]) {
        autoreleasepool {
            do {
                let c0 = dirSizeMiB(e5CacheURL)
                let t0 = nowNs()
                let m = try loadModel(name, function: function)
                let loadMs = Double(nowNs() - t0) / 1e6
                let input = try makeInput(rows: t, x: x)
                let t1 = nowNs()
                _ = try predict(m, input)
                let firstUs = Double(nowNs() - t1) / 1e3
                let c1 = dirSizeMiB(e5CacheURL)
                let t2 = nowNs()
                let m2 = try loadModel(name, function: function)
                let reloadMs = Double(nowNs() - t2) / 1e6
                _ = try predict(m2, input)
                print(String(format: "| %@ | %@ | **%.0f** | %.0f → %.0f | %.0f | %.0f |", name, function ?? "main",
                             loadMs, c0, c1, firstUs, reloadMs))
            } catch {
                print("| \(name) | \(function ?? "main") | błąd: \(error) | | | |")
            }
        }
    }
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        loadTimed(multiName, function: "T\(t)", t: t, x: x)
    }
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        loadTimed("ffn_T\(t)_\(multiVariant)", function: nil, t: t, x: x)
    }
    print("")
    print("| T | wariant | mediana [µs] | IQR | ważny | TFLOPS | multi wobec osobnego |")
    print("|--:|---|--:|--:|---|--:|--:|")
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        autoreleasepool {
            do {
                let sep = try loadModel("ffn_T\(t)_\(multiVariant)")
                let multi = try loadModel(multiName, function: "T\(t)")
                let input = try makeInput(rows: t, x: x)
                let res = try measureInterleaved([{ _ = try predict(sep, input) }, { _ = try predict(multi, input) }])
                print(String(format: "| %d | osobny model | **%.0f** | %.1f%% | %@ | %.2f | — |", t, res[0].us, res[0].spread,
                             validMark(res[0].spread), tflops(t, res[0].us)))
                print(String(format: "| %d | multifunction T%d | **%.0f** | %.1f%% | %@ | %.2f | %+.1f%% |", t, t, res[1].us,
                             res[1].spread, validMark(res[1].spread), tflops(t, res[1].us), (res[1].us / res[0].us - 1) * 100))
            } catch {
                print("| \(t) | błąd: \(error) | | | | | |")
            }
        }
        pause(5)
    }
    print("")
}

// ============================================================ 0.2 pamięć

if want("multimem") || want("multimem-rev") {
    let reverse = section == "multimem-rev"
    print("## 0.2 Pamięć: 3 funkcje multifunction wobec 3 osobnych modeli (\(multiVariant), kolejność: \(reverse ? "osobne → multi" : "multi → osobne"))")
    print("")
    print("Każdy wiersz: przyrost wobec stanu sprzed załadowania danej trójki, po jednym predict")
    print("na każdej funkcji (bufory we/wy alokowane wcześniej, poza pomiarem). vm_stat w MiB.")
    print("")
    print("| zestaw | Δ po załadowaniu 3 | Δ po zwolnieniu |")
    print("|---|---|---|")
    let xs = shapesT.map { loadF16(modelsDir.appendingPathComponent("x_T\($0).f16.bin")) }
    let inputs = try! zip(shapesT, xs).map { try makeInput(rows: $0, x: $1) }
    func run(_ label: String, _ loader: (Int) throws -> MLModel) {
        pause(3)
        let b = SysMem.now()
        var models: [MLModel] = []
        autoreleasepool {
            do {
                for (i, t) in shapesT.enumerated() {
                    let m = try loader(t)
                    _ = try predict(m, inputs[i])
                    models.append(m)
                }
            } catch { print("| \(label) | błąd: \(error) | |") }
        }
        pause(2)
        let a = SysMem.now()
        models.removeAll()
        pause(3)
        let r = SysMem.now()
        print("| \(label) | \(a.delta(b)) | \(r.delta(b)) |")
    }
    let multi = { (t: Int) in try loadModel("ffn_multi_\(multiVariant)", function: "T\(t)") }
    let sep = { (t: Int) in try loadModel("ffn_T\(t)_\(multiVariant)") }
    if reverse {
        run("3 osobne modele", sep)
        run("multifunction ×3 funkcje", multi)
    } else {
        run("multifunction ×3 funkcje", multi)
        run("3 osobne modele", sep)
    }
    print("")
}

// ============================================================ 0.3 wyjście z krokiem

if want("strided") {
    print("## 0.3 Wyjście z krokiem: outputBackings na buforze [T, \(rowsTotal)] (wynik [T, \(dModel)]), \(stridedVariant)")
    print("")
    print("Bufor szeroki wypełniony wartownikiem; wynik ma wylądować w kolumnach [colOffset, colOffset+4096).")
    print("„in-place” = wskaźnik danych zwróconej tablicy równy naszemu (CoreML nie podmienił bufora).")
    print("„poza zakresem nietknięte” = wszystkie komórki poza oknem nadal równe wartownikowi.")
    print("Czas: przeplatany z wyjściem kontigualnym (a2) i z (a2)+ręczną kopią wierszy do bufora szerokiego.")
    print("")
    print("| T | wariant | akceptacja | in-place | bajty = (a2) | poza zakresem nietknięte | mediana [µs] | IQR | ważny | wobec (a2) |")
    print("|--:|---|---|---|---|---|--:|--:|---|--:|")
    let sentinel = Float16(-777)
    for t in shapesT {
        let x = loadF16(modelsDir.appendingPathComponent("x_T\(t).f16.bin"))
        autoreleasepool {
            do {
                let model = try loadModel("ffn_T\(t)_\(stridedVariant)")
                let input = try makeInput(rows: t, x: x)
                let shape: [NSNumber] = [NSNumber(value: t), NSNumber(value: dModel)]

                // (a2) kontigualne wyjście na buforze wyrównanym do strony.
                var pc: UnsafeMutableRawPointer? = nil
                posix_memalign(&pc, 16384, t * dModel * 2)
                let contig = try MLMultiArray(dataPointer: pc!, shape: shape, dataType: .float16,
                                              strides: [NSNumber(value: dModel), 1], deallocator: nil)
                let optsC = MLPredictionOptions()
                optsC.outputBackings = ["y": contig]
                let outC = try predict(model, input, options: optsC).featureValue(for: "y")!.multiArrayValue!
                let contigInPlace = outC.dataPointer == pc!
                let refBytes = pc!.bindMemory(to: UInt16.self, capacity: t * dModel)

                // Bufor szeroki [T, rowsTotal] i dwa okna: colOffset 0 (początek wiersza wyrównany
                // do strony) i colOffset 4096 (środek wiersza, 8 KiB od początku strony).
                var pw: UnsafeMutableRawPointer? = nil
                posix_memalign(&pw, 16384, t * rowsTotal * 2)
                let wide = pw!.bindMemory(to: Float16.self, capacity: t * rowsTotal)
                let wideBits = pw!.bindMemory(to: UInt16.self, capacity: t * rowsTotal)
                func fillSentinel() { for i in 0..<(t * rowsTotal) { wide[i] = sentinel } }
                func check(colOffset: Int) -> (same: Bool, untouched: Bool) {
                    var same = true, untouched = true
                    for r in 0..<t {
                        for c in 0..<rowsTotal {
                            let v = wideBits[r * rowsTotal + c]
                            if c >= colOffset && c < colOffset + dModel {
                                if v != refBytes[r * dModel + (c - colOffset)] { same = false }
                            } else if v != sentinel.bitPattern { untouched = false }
                        }
                    }
                    return (same, untouched)
                }

                var bodies: [() throws -> Void] = [{ _ = try predict(model, input, options: optsC) }]
                var labels = ["(a2) kontigualne + outputBackings"]
                var flags = [(accepted: "tak", inPlace: contigInPlace ? "tak" : "NIE", same: "—", untouched: "—")]

                // (a2) + ręczna kopia wierszy do bufora szerokiego (alternatywa, gdy krok nie działa).
                bodies.append {
                    _ = try predict(model, input, options: optsC)
                    for r in 0..<t {
                        memcpy(pw! + (r * rowsTotal + 4096) * 2, pc! + r * dModel * 2, dModel * 2)
                    }
                }
                labels.append("(a2) + memcpy \(t) wierszy do [T,\(rowsTotal)]")
                flags.append(("tak", "—", "—", "—"))

                for colOffset in [0, 4096] {
                    fillSentinel()
                    let strided = try MLMultiArray(dataPointer: pw! + colOffset * 2, shape: shape, dataType: .float16,
                                                   strides: [NSNumber(value: rowsTotal), 1], deallocator: nil)
                    let opts = MLPredictionOptions()
                    opts.outputBackings = ["y": strided]
                    var accepted = "tak", inPlace = "—", same = "—", untouched = "—"
                    do {
                        let out = try predict(model, input, options: opts).featureValue(for: "y")!.multiArrayValue!
                        inPlace = (out.dataPointer == pw! + colOffset * 2) ? "tak" : "NIE (CoreML dał własny bufor)"
                        let chk = check(colOffset: colOffset)
                        same = chk.same ? "tak" : "NIE"
                        untouched = chk.untouched ? "tak" : "NIE"
                        if !chk.same {
                            // Czy wynik w ogóle trafił do naszego bufora?
                            var anyWritten = false
                            for i in 0..<(t * rowsTotal) where wideBits[i] != sentinel.bitPattern { anyWritten = true; break }
                            same += anyWritten ? " (bufor zapisany, ale inaczej)" : " (bufor nietknięty — wynik tylko w zwróconej tablicy)"
                            // Sprawdź zwróconą tablicę wobec (a2).
                            let pr = out.dataPointer.bindMemory(to: UInt16.self, capacity: 1)
                            var retSame = true
                            let rs = out.strides[0].intValue
                            for r in 0..<t { for c in 0..<dModel where pr[r * rs + c] != refBytes[r * dModel + c] { retSame = false; break } }
                            same += retSame ? "; zwrócona tablica = (a2)" : "; zwrócona tablica ≠ (a2)"
                        }
                        bodies.append { _ = try predict(model, input, options: opts) }
                    } catch {
                        accepted = "NIE: \(error)"
                        bodies.append { }
                    }
                    labels.append("krok \(rowsTotal), colOffset \(colOffset)")
                    flags.append((accepted, inPlace, same, untouched))
                }

                let res = try measureInterleaved(bodies)
                for i in 0..<bodies.count {
                    let f = flags[i]
                    print(String(format: "| %d | %@ | %@ | %@ | %@ | %@ | **%.0f** | %.1f%% | %@ | %+.1f%% |", t, labels[i],
                                 f.accepted, f.inPlace, f.same, f.untouched, res[i].us, res[i].spread,
                                 validMark(res[i].spread), (res[i].us / res[0].us - 1) * 100))
                }
            } catch {
                print("| \(t) | błąd: \(error) | | | | | | | | |")
            }
        }
        pause(5)
    }
    print("")
}

logTherm("koniec")

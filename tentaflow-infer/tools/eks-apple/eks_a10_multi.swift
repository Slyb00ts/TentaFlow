// =============================================================================
// Plik: eks_a10_multi.swift
// Opis: EKS-A10 — diagnoza spowolnienia predict przy WIELU załadowanych modelach
//       ANE (80 mlmodelc warstw FFN Bielika). Ładuje N modeli (T1024, cpuAndNeuralEngine),
//       mierzy per predict czas ścienny, czas CPU procesu (getrusage) i czas CPU
//       wątków (thread_info), zlicza ostrzeżenia "E5 bundle" (E5RT pisze je na
//       stdout ORAZ stderr — liczone w obu strumieniach potomka), robi
//       MLComputePlan po załadowaniu wszystkich, sprawdza vm_stat i RSS aned.
//       Kontrole: T256, 80× ten sam plik, 80 kopii pliku, mlpackage kompilowane
//       on-device, computeUnits all, opcje MLModelConfiguration.
// Przykład: ./run.sh a10multi <katalog_modeli> sweep            # N = 1,2,5,10,20,40,80, każde w nowym procesie
//           ./run.sh a10multi <katalog_modeli> run n=80 plan=1  # jeden przebieg, z MLComputePlan
//           opcje: n=<N> fn=T1024|T256 units=ane|all source=layers|same-path|copies|compiled
//                  pkg=<katalog_mlpackage> lowprec=1 plan=1 stderr=<plik> ns=1,2,5
// =============================================================================

import CoreML
import Foundation

// ---------------------------------------------------------------- parametry

let warmupPerModel = 3         // rozgrzewka: tyle predict na każdym modelu
let rounds = 5                 // rund round-robin po wszystkich N modelach
let defaultNs = [1, 2, 5, 10, 20, 40, 80]
let dModel = 4096
let interSlice = 11264         // K wejścia down (pełny inter; wycinek ANE liczy 2432 wierszy)
let gateUpWidth = 13440        // wyjście gate_up (2 × 6720)
let downWidth = 2432           // wyjście down

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

func shell(_ path: String, _ args: [String]) -> String {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    p.arguments = args
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = FileHandle.nullDevice
    try? p.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    return String(data: data, encoding: .utf8) ?? ""
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

/// Czas CPU całego procesu (user+system) w µs — getrusage(RUSAGE_SELF).
func processCpuUs() -> Double {
    let u = rusageNow()
    return u.userUs + u.sysUs
}

/// Zrzut getrusage: czas user/system [µs] i liczba błędów stron (drobne = mapowanie/zero-fill, duże = z dysku/swapu).
struct Usage {
    var userUs = 0.0, sysUs = 0.0, minflt = 0.0, majflt = 0.0
}
func rusageNow() -> Usage {
    var r = rusage()
    getrusage(RUSAGE_SELF, &r)
    return Usage(userUs: Double(r.ru_utime.tv_sec) * 1e6 + Double(r.ru_utime.tv_usec),
                 sysUs: Double(r.ru_stime.tv_sec) * 1e6 + Double(r.ru_stime.tv_usec),
                 minflt: Double(r.ru_minflt), majflt: Double(r.ru_majflt))
}

/// Czas CPU każdego żywego wątku (user+system, µs) z nazwą pthread — thread_info(THREAD_BASIC_INFO).
func threadCpuUs() -> [UInt32: (name: String, us: Double)] {
    var list: thread_act_array_t? = nil
    var count: mach_msg_type_number_t = 0
    guard task_threads(mach_task_self_, &list, &count) == KERN_SUCCESS, let threads = list else { return [:] }
    var out: [UInt32: (name: String, us: Double)] = [:]
    for i in 0..<Int(count) {
        let th = threads[i]
        var info = thread_basic_info()
        var icount = mach_msg_type_number_t(MemoryLayout<thread_basic_info>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(icount)) {
                thread_info(th, thread_flavor_t(THREAD_BASIC_INFO), $0, &icount)
            }
        }
        if kr == KERN_SUCCESS {
            let us = Double(info.user_time.seconds + info.system_time.seconds) * 1e6
                + Double(info.user_time.microseconds + info.system_time.microseconds)
            var name = [CChar](repeating: 0, count: 128)
            if let p = pthread_from_mach_thread_np(th) { pthread_getname_np(p, &name, 128) }
            var label = String(cString: name)
            if label.isEmpty { label = (i == 0) ? "main" : "(bez nazwy)" }
            out[th] = (label, us)
        }
        mach_port_deallocate(mach_task_self_, th)
    }
    vm_deallocate(mach_task_self_, vm_address_t(bitPattern: threads),
                  vm_size_t(count) * vm_size_t(MemoryLayout<thread_t>.size))
    return out
}

/// Różnica czasów CPU wątków między dwoma zrzutami, zsumowana po nazwie wątku, posortowana malejąco.
func threadDelta(_ a: [UInt32: (name: String, us: Double)], _ b: [UInt32: (name: String, us: Double)]) -> [(String, Double)] {
    var byName: [String: Double] = [:]
    for (port, cur) in b {
        let prev = a[port]?.us ?? 0
        let d = max(0, cur.us - prev)
        if d > 0 { byName[cur.name, default: 0] += d }
    }
    return byName.sorted { $0.value > $1.value }
}

/// Pamięć procesu: phys_footprint z task_info.
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

/// vm_stat (strony 16 KiB) + RSS aned + footprint procesu, w MiB.
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
        m.pageins = pages("Pageins"); m.pageouts = pages("Pageouts")
        m.swapins = pages("Swapins"); m.swapouts = pages("Swapouts")
        m.decompressions = pages("Decompressions")
        let pid = shell("/usr/bin/pgrep", ["-x", "aned"]).trimmingCharacters(in: .whitespacesAndNewlines)
        if !pid.isEmpty {
            let rss = shell("/bin/ps", ["-o", "rss=", "-p", pid]).trimmingCharacters(in: .whitespacesAndNewlines)
            m.anedRssMiB = (Double(rss) ?? 0) / 1024
        }
        m.procMiB = footprintMiB()
        // sysctl vm.swapusage: "total = 4096,00M  used = 3179,31M  free = …"
        let sw = shell("/usr/sbin/sysctl", ["-n", "vm.swapusage"])
        if let r = sw.range(of: "used = ") {
            let rest = sw[r.upperBound...].prefix { $0 != "M" }.replacingOccurrences(of: ",", with: ".")
            m.swapUsedMiB = Double(rest) ?? 0
        }
        return m
    }
    var swapUsedMiB = 0.0
    var pageins = 0.0, pageouts = 0.0, swapins = 0.0, swapouts = 0.0, decompressions = 0.0
    /// Liczniki vm_stat od zrzutu `b` (strony 16 KiB): ile stron wczytano z dysku/swapu/kompresora.
    func vmDelta(_ b: SysMem) -> String {
        String(format: "pageins %+.0f, pageouts %+.0f, swapins %+.0f, swapouts %+.0f, dekompresje %+.0f",
               pageins - b.pageins, pageouts - b.pageouts, swapins - b.swapins, swapouts - b.swapouts, decompressions - b.decompressions)
    }
    var text: String {
        String(format: "proces %.0f, aned RSS %.0f, wired %.0f, wolne %.0f, kompresor %.0f, swap użyty %.0f",
               procMiB, anedRssMiB, wiredMiB, freeMiB, compressorMiB, swapUsedMiB)
    }
}

// --------------------------------------------------------------- argumenty

let args = CommandLine.arguments
guard args.count >= 3 else {
    print("użycie: eks_a10_multi <katalog_modeli> sweep|sweep-inproc|run [klucz=wartość …]")
    exit(2)
}
let modelsDir = URL(fileURLWithPath: args[1])
let mode = args[2]
var opts: [String: String] = [:]
for a in args.dropFirst(3) {
    let kv = a.split(separator: "=", maxSplits: 1).map(String.init)
    opts[kv[0]] = kv.count > 1 ? kv[1] : "1"
}
let fnName = opts["fn"] ?? "T1024"
let tokens = Int(fnName.dropFirst()) ?? 1024
let unitsOpt = opts["units"] ?? "ane"
let source = opts["source"] ?? "layers"          // layers | same-path | copies | compiled
let lowPrec = opts["lowprec"] == "1"
let sharedBackings = opts["backings"] == "1"
let ballastMiB = Int(opts["ballast"] ?? "0") ?? 0   // balast: tyle MiB pamięci trzymanej i przemiatanej przez osobny wątek (symulacja wag silnika)   // jeden wspólny bufor wyjściowy na rodzaj (outputBackings) dla wszystkich N modeli
let wantPlan = opts["plan"] == "1"
let nModels = Int(opts["n"] ?? "80") ?? 80
let outDir = URL(fileURLWithPath: opts["out"] ?? NSTemporaryDirectory()).appendingPathComponent("eks_a10_multi")
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

// ------------------------------------------------------------------ balast

/// Wątek „ballast”: alokuje ballastMiB, zapisuje całość, potem w pętli przemiata po 64 MiB (memset)
/// z krótkimi przerwami — jak silnik, który równolegle trzyma i czyta swoje wagi CPU/GPU.
func startBallast() {
    guard ballastMiB > 0 else { return }
    let ready = DispatchSemaphore(value: 0)
    let t = Thread {
        let size = ballastMiB * 1048576
        var p: UnsafeMutableRawPointer? = nil
        posix_memalign(&p, 16384, size)
        guard let base = p else { ready.signal(); return }
        memset(base, 1, size)
        ready.signal()
        let chunk = 64 * 1048576
        while true {
            var off = 0
            while off < size {
                memset(base + off, Int32(off & 0xff), min(chunk, size - off))
                off += chunk
                usleep(2000)
            }
        }
    }
    t.name = "ballast"
    t.start()
    ready.wait()
}

// ------------------------------------------------------------------ stderr

/// Przekierowanie fd 2 do pliku (CoreML pisze tam ostrzeżenia E5) i zliczanie wzorców.
var stderrFile: URL? = nil
func redirectStderr(to url: URL) {
    let fd = open(url.path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
    guard fd >= 0 else { return }
    dup2(fd, 2)
    close(fd)
    stderrFile = url
}

struct E5Count { var bundle = 0, loadFailed = 0 }
func countE5() -> E5Count {
    guard let f = stderrFile, let s = try? String(contentsOf: f, encoding: .utf8) else { return E5Count() }
    return E5Count(bundle: s.components(separatedBy: "E5 bundle").count - 1,
                   loadFailed: s.components(separatedBy: "ANE model load has failed").count - 1)
}

// ------------------------------------------------------------------ CoreML

enum Kind: String { case gateUp = "gate_up", down = "down" }

func computeUnits() -> MLComputeUnits { unitsOpt == "all" ? .all : .cpuAndNeuralEngine }

func config(function: String?, name: String) -> MLModelConfiguration {
    let cfg = MLModelConfiguration()
    cfg.computeUnits = computeUnits()
    if let f = function { cfg.functionName = f }
    if lowPrec {
        cfg.allowLowPrecisionAccumulationOnGPU = true
        cfg.modelDisplayName = name
    }
    return cfg
}

/// Opis i-tego modelu do załadowania w zależności od źródła.
struct Spec {
    let name: String       // etykieta
    let kind: Kind
    let url: URL           // .mlmodelc (albo .mlpackage dla source=compiled)
}

func specs(_ n: Int) -> [Spec] {
    var out: [Spec] = []
    for i in 0..<n {
        let layer = i / 2
        let kind: Kind = i % 2 == 0 ? .gateUp : .down
        let base = String(format: "L%02d_%@", layer, kind.rawValue)
        switch source {
        case "same-path":
            out.append(Spec(name: "L00_gate_up#\(i)", kind: .gateUp, url: modelsDir.appendingPathComponent("L00_gate_up.mlmodelc")))
        case "copies":
            // kopie (APFS clone) w outDir/copies/L00_gate_up_XX.mlmodelc — tworzone przez rodzica
            out.append(Spec(name: "L00_gate_up_kopia\(i)", kind: .gateUp,
                            url: outDir.appendingPathComponent(String(format: "copies/L00_gate_up_%02d.mlmodelc", i))))
        case "compiled":
            let pkg = URL(fileURLWithPath: opts["pkg"] ?? modelsDir.appendingPathComponent("mlpackage").path)
            out.append(Spec(name: base, kind: kind, url: pkg.appendingPathComponent("\(base).mlpackage")))
        default:
            out.append(Spec(name: base, kind: kind, url: modelsDir.appendingPathComponent("\(base).mlmodelc")))
        }
    }
    return out
}

var compileMs: [Double] = []

func loadModel(_ s: Spec) throws -> MLModel {
    var url = s.url
    if source == "compiled" {
        let t0 = nowNs()
        url = try MLModel.compileModel(at: s.url)   // on-device kompilacja do katalogu tymczasowego
        compileMs.append(Double(nowNs() - t0) / 1e6)
    }
    return try MLModel(contentsOf: url, configuration: config(function: fnName, name: s.name))
}

/// Wejście f16 [T, K] na buforze wyrównanym do strony, wartości pseudolosowe w [-1, 1].
func makeInput(rows: Int, cols: Int, seed: UInt64) throws -> MLMultiArray {
    var p: UnsafeMutableRawPointer? = nil
    posix_memalign(&p, 16384, rows * cols * 2)
    let pi = p!.bindMemory(to: Float16.self, capacity: rows * cols)
    var st = seed &+ 0x9E3779B97F4A7C15
    for i in 0..<(rows * cols) {
        st ^= st << 13; st ^= st >> 7; st ^= st << 17
        pi[i] = Float16(Double(st % 20001) / 10000.0 - 1.0)
    }
    return try MLMultiArray(dataPointer: p!, shape: [NSNumber(value: rows), NSNumber(value: cols)],
                            dataType: .float16, strides: [NSNumber(value: cols), 1], deallocator: nil)
}

var outputBackings: [Kind: MLMultiArray] = [:]

func predict(_ model: MLModel, _ input: MLMultiArray, kind: Kind = .gateUp) throws {
    let provider = try MLDictionaryFeatureProvider(dictionary: ["x": MLFeatureValue(multiArray: input)])
    if sharedBackings, let y = outputBackings[kind] {
        let o = MLPredictionOptions()
        o.outputBackings = ["y": y]
        _ = try model.prediction(from: provider, options: o)
        return
    }
    _ = try model.prediction(from: provider)
}

func flops(_ k: Kind) -> Double {
    switch k {
    case .gateUp: return 2.0 * Double(tokens) * Double(dModel) * Double(gateUpWidth)
    case .down: return 2.0 * Double(tokens) * Double(interSlice) * Double(downWidth)
    }
}

func tflops(_ k: Kind, ms: Double) -> Double { ms > 0 ? flops(k) / (ms * 1e-3) / 1e12 : 0 }

// ------------------------------------------------------------ MLComputePlan

/// Zwraca (linear na ANE, linear na CPU, linear na GPU, koszt ANE %) dla funkcji programu.
func computePlan(_ s: Spec, compiledURL: URL) -> (ane: Int, cpu: Int, gpu: Int, costANE: Double, err: String?) {
    let sem = DispatchSemaphore(value: 0)
    var result = (ane: 0, cpu: 0, gpu: 0, costANE: 0.0, err: nil as String?)
    let cfg = config(function: fnName, name: s.name)
    Task.detached {
        defer { sem.signal() }
        do {
            let plan = try await MLComputePlan.load(contentsOf: compiledURL, configuration: cfg)
            guard case let .program(program) = plan.modelStructure, let fn = program.functions[fnName] else {
                result.err = "brak funkcji \(fnName)"
                return
            }
            var costAll = 0.0, costANE = 0.0
            for op in fn.block.operations {
                let cost = plan.estimatedCost(of: op)?.weight ?? 0
                costAll += cost
                guard let usage = plan.deviceUsage(for: op) else { continue }
                if case .neuralEngine = usage.preferred { costANE += cost }
                if op.operatorName == "linear" {
                    switch usage.preferred {
                    case .neuralEngine: result.ane += 1
                    case .cpu: result.cpu += 1
                    case .gpu: result.gpu += 1
                    @unknown default: break
                    }
                }
            }
            result.costANE = costAll > 0 ? costANE / costAll * 100 : 0
        } catch {
            result.err = "\(error)"
        }
    }
    sem.wait()
    return result
}

// ================================================================== run

/// Jeden przebieg: N modeli, rozgrzewka, 5 rund round-robin, pomiar wall/CPU, E5, pamięć, plan.
/// Wypisuje sekcję Markdown oraz linie `RES|…` do agregacji przez tryb sweep.
func runOnce(n: Int, label: String) {
    let list = specs(n)
    print("### \(label): N=\(n), fn=\(fnName), units=\(unitsOpt), źródło=\(source)\(lowPrec ? ", lowprec+displayName" : "")\(sharedBackings ? ", wspólne outputBackings" : "")\(ballastMiB > 0 ? ", balast \(ballastMiB) MiB" : "")")
    print("")
    let memBefore = SysMem.now()
    print("- pamięć przed ładowaniem [MiB]: \(memBefore.text)")

    var models: [MLModel] = []
    var loadMs: [Double] = []
    var firstE5: Int? = nil
    var e5AfterEach: [Int] = []
    let cpuLoad0 = processCpuUs()
    let uLoad0 = rusageNow()
    let tLoad0 = nowNs()
    for (i, s) in list.enumerated() {
        let t0 = nowNs()
        do {
            models.append(try loadModel(s))
        } catch {
            print("- BŁĄD ładowania \(s.name) (model \(i + 1)/\(n)): \(error)")
            print("RES|\(label)|\(n)|error|\(error)")
            return
        }
        loadMs.append(Double(nowNs() - t0) / 1e6)
        let e = countE5()
        e5AfterEach.append(e.bundle)
        if firstE5 == nil && (e.bundle > 0 || e.loadFailed > 0) { firstE5 = i + 1 }
    }
    let loadTotalMs = Double(nowNs() - tLoad0) / 1e6
    let loadCpuMs = (processCpuUs() - cpuLoad0) / 1e3
    let e5Load = countE5()
    print(String(format: "- ładowanie %d modeli: %.0f ms ścienne (mediana %.1f ms/model, max %.1f), CPU procesu %.0f ms%@",
                 n, loadTotalMs, median(loadMs), loadMs.max() ?? 0, loadCpuMs,
                 compileMs.isEmpty ? "" : String(format: "; kompilacja on-device mediana %.0f ms/model", median(compileMs))))
    print("- ostrzeżenia po ładowaniu: „E5 bundle” ×\(e5Load.bundle), „ANE model load has failed” ×\(e5Load.loadFailed)"
          + (firstE5.map { "; pierwsze przy modelu nr \($0)" } ?? "; brak"))
    let memAfterLoad = SysMem.now()
    let uLoad1 = rusageNow()
    print(String(format: "- błędy stron podczas ładowania: drobne %.0f, duże %.0f", uLoad1.minflt - uLoad0.minflt, uLoad1.majflt - uLoad0.majflt))
    print("- pamięć po załadowaniu [MiB]: \(memAfterLoad.text); vm_stat od startu: \(memAfterLoad.vmDelta(memBefore))")

    // wejścia: jedno na rodzaj
    let inputs: [Kind: MLMultiArray] = [
        .gateUp: try! makeInput(rows: tokens, cols: dModel, seed: 1),
        .down: try! makeInput(rows: tokens, cols: interSlice, seed: 2),
    ]

    if sharedBackings {
        outputBackings[.gateUp] = try! makeInput(rows: tokens, cols: gateUpWidth, seed: 3)
        outputBackings[.down] = try! makeInput(rows: tokens, cols: downWidth, seed: 4)
    }
    // rozgrzewka
    let cpuWarm0 = processCpuUs()
    let uWarm0 = rusageNow()
    let tWarm0 = nowNs()
    for (i, m) in models.enumerated() {
        for _ in 0..<warmupPerModel {
            do { try predict(m, inputs[list[i].kind]!, kind: list[i].kind) } catch {
                print("- BŁĄD predict (rozgrzewka) \(list[i].name): \(error)")
                print("RES|\(label)|\(n)|error|\(error)")
                return
            }
        }
    }
    let uWarm1 = rusageNow()
    print(String(format: "- rozgrzewka %d×%d predict: %.0f ms ścienne, CPU procesu %.0f ms (system %.0f), błędy stron drobne %.0f, duże %.0f",
                 n, warmupPerModel, Double(nowNs() - tWarm0) / 1e6, (processCpuUs() - cpuWarm0) / 1e3,
                 (uWarm1.sysUs - uWarm0.sysUs) / 1e3, uWarm1.minflt - uWarm0.minflt, uWarm1.majflt - uWarm0.majflt))

    // pomiar: 5 rund round-robin; per predict wall i CPU procesu
    var wall = [[Double]](repeating: [], count: n)
    var cpu = [[Double]](repeating: [], count: n)
    var sys = [[Double]](repeating: [], count: n)
    var minflt = [[Double]](repeating: [], count: n)
    var majflt = [[Double]](repeating: [], count: n)
    var roundWall: [Double] = []
    let thr0 = threadCpuUs()
    let cpuMeas0 = processCpuUs()
    let tMeas0 = nowNs()
    for _ in 0..<rounds {
        let tr = nowNs()
        for (i, m) in models.enumerated() {
            let u0 = rusageNow()
            let t0 = nowNs()
            try! predict(m, inputs[list[i].kind]!, kind: list[i].kind)
            let t1 = nowNs()
            let u1 = rusageNow()
            wall[i].append(Double(t1 - t0) / 1e6)
            cpu[i].append((u1.userUs + u1.sysUs - u0.userUs - u0.sysUs) / 1e3)
            sys[i].append((u1.sysUs - u0.sysUs) / 1e3)
            minflt[i].append(u1.minflt - u0.minflt)
            majflt[i].append(u1.majflt - u0.majflt)
        }
        roundWall.append(Double(nowNs() - tr) / 1e6)
    }
    let measWallMs = Double(nowNs() - tMeas0) / 1e6
    let measCpuMs = (processCpuUs() - cpuMeas0) / 1e3
    let thr1 = threadCpuUs()
    let e5After = countE5()
    let memAfterPred = SysMem.now()
    print("- pamięć po predictach [MiB]: \(memAfterPred.text); vm_stat od załadowania: \(memAfterPred.vmDelta(memAfterLoad))")
    print(String(format: "- pomiar %d rund × %d predict: %.0f ms ścienne, CPU procesu %.0f ms (%.2f rdzenia średnio); ostrzeżenia E5 w trakcie predict: +%d",
                 rounds, n, measWallMs, measCpuMs, measWallMs > 0 ? measCpuMs / measWallMs : 0, e5After.bundle - e5Load.bundle))
    print(String(format: "- pełne przejście po %d modelach (jedna runda): mediana %.1f ms, min %.1f, max %.1f",
                 n, median(roundWall), roundWall.min() ?? 0, roundWall.max() ?? 0))

    // wątki: kto zużył CPU w oknie pomiaru
    let td = threadDelta(thr0, thr1)
    let top = td.prefix(6).map { String(format: "%@ %.0f ms", $0.0, $0.1 / 1e3) }.joined(separator: ", ")
    print("- czas CPU wątków w oknie pomiaru (suma \(String(format: "%.0f", td.reduce(0) { $0 + $1.1 } / 1e3)) ms): \(top)")

    // per rodzaj
    print("")
    print("| rodzaj | modeli | wall mediana [ms] | IQR | wall max | CPU proc. mediana [ms] | w tym system [ms] | CPU/wall | błędy stron drobne/duże na predict (mediana) | TFLOPS (mediana) |")
    print("|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
    var resKinds: [String] = []
    for kind in [Kind.gateUp, Kind.down] {
        let idx = (0..<n).filter { list[$0].kind == kind }
        if idx.isEmpty { continue }
        let w = idx.flatMap { wall[$0] }
        let c = idx.flatMap { cpu[$0] }
        let mw = median(w), mc = median(c)
        print(String(format: "| %@ | %d | **%.2f** | %.1f%% | %.2f | %.2f | %.2f | %.2f | %.0f / %.0f | **%.2f** |", kind.rawValue, idx.count, mw,
                     mw > 0 ? iqr(w) / mw * 100 : 0, w.max() ?? 0, mc, median(idx.flatMap { sys[$0] }), mw > 0 ? mc / mw : 0,
                     median(idx.flatMap { minflt[$0] }), median(idx.flatMap { majflt[$0] }), tflops(kind, ms: mw)))
        resKinds.append(String(format: "%@:%.2f:%.2f:%.2f", kind.rawValue, mw, mc, tflops(kind, ms: mw)))
    }
    print("")

    // które modele są wolne (mediana per model wobec najszybszego tego samego rodzaju)
    var slow: [String] = []
    for kind in [Kind.gateUp, Kind.down] {
        let idx = (0..<n).filter { list[$0].kind == kind }
        guard let best = idx.map({ median(wall[$0]) }).min(), best > 0 else { continue }
        for i in idx where median(wall[i]) > 1.5 * best {
            slow.append(String(format: "%@ (nr %d) %.1f ms, CPU %.1f ms", list[i].name, i + 1, median(wall[i]), median(cpu[i])))
        }
    }
    print("- modele wolniejsze niż 1,5× najszybszy tego rodzaju: " + (slow.isEmpty ? "brak" : "\(slow.count) — " + slow.prefix(12).joined(separator: "; ") + (slow.count > 12 ? "; …" : "")))
    let perModel = (0..<n).map { String(format: "%.1f/%.1f", median(wall[$0]), median(cpu[$0])) }.joined(separator: " ")
    print("- per model (mediana wall/CPU ms, kolejność ładowania): \(perModel)")

    // MLComputePlan po załadowaniu wszystkich
    if wantPlan {
        var aneAll = 0, cpuAny = 0, errs = 0
        var cpuNames: [String] = []
        let tp0 = nowNs()
        for (i, s) in list.enumerated() {
            let url = source == "compiled" ? ((try? MLModel.compileModel(at: s.url)) ?? s.url) : s.url
            let p = computePlan(s, compiledURL: url)
            if p.err != nil { errs += 1; continue }
            if p.cpu == 0 && p.gpu == 0 { aneAll += 1 } else { cpuAny += 1; cpuNames.append("\(s.name) (nr \(i + 1): ANE \(p.ane), CPU \(p.cpu), GPU \(p.gpu))") }
        }
        print(String(format: "- MLComputePlan (%@) po załadowaniu wszystkich %d: linear w 100%% na ANE: %d modeli; z linear na CPU/GPU: %d; błędy: %d (%.0f s)%@",
                     fnName, n, aneAll, cpuAny, errs, Double(nowNs() - tp0) / 1e9,
                     cpuNames.isEmpty ? "" : " — " + cpuNames.prefix(10).joined(separator: "; ")))
        let e5Plan = countE5()
        print("- ostrzeżenia E5 po MLComputePlan: „E5 bundle” ×\(e5Plan.bundle) (przyrost +\(e5Plan.bundle - e5After.bundle))")
    }

    // zwolnienie (kontrola: czy aned oddaje pamięć, czy kolejny zestaw ładuje się bez ostrzeżeń)
    print("- stan termiczny: \(thermalState())")
    print("RES|\(label)|\(n)|ok|\(resKinds.joined(separator: ";"))|e5=\(e5After.bundle)|fail=\(e5After.loadFailed)|first=\(firstE5 ?? 0)|round=\(String(format: "%.1f", median(roundWall)))|aned=\(String(format: "%.0f", memAfterPred.anedRssMiB))|proc=\(String(format: "%.0f", memAfterPred.procMiB))|cpushare=\(String(format: "%.2f", measWallMs > 0 ? measCpuMs / measWallMs : 0))")
    print("")
    models.removeAll()
}

// ================================================================== sweep

/// Uruchamia samego siebie w nowym procesie dla każdego N; przechwytuje stdout, agreguje RES.
func sweep(ns: [Int]) -> [String] {
    var res: [String] = []
    let exe = URL(fileURLWithPath: args[0]).standardizedFileURL
    for n in ns {
        let p = Process()
        p.executableURL = exe
        let errFile = outDir.appendingPathComponent("stderr_\(source)_\(fnName)_\(unitsOpt)_n\(n).txt")
        var childArgs = [modelsDir.path, "run", "n=\(n)", "stderr=\(errFile.path)"]
        for (k, v) in opts where !["n", "ns", "stderr"].contains(k) { childArgs.append("\(k)=\(v)") }
        p.arguments = childArgs
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = FileHandle.standardError
        do { try p.run() } catch { print("nie mogę uruchomić potomka: \(error)"); continue }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        let out = String(data: data, encoding: .utf8) ?? ""
        print(out, terminator: "")
        if p.terminationStatus != 0 { print("- potomek N=\(n) zakończył się kodem \(p.terminationStatus)\n") }
        for line in out.split(separator: "\n") where line.hasPrefix("RES|") { res.append(String(line)) }
        // E5RT pisze komunikaty na fd 1 (stdout), nie tylko na fd 2 — zlicz je w stdout potomka,
        // pomijając własne linie harnessu (zaczynają się od "- ", "|", "#", "RES|").
        let e5Stdout = out.split(separator: "\n").filter {
            !$0.hasPrefix("- ") && !$0.hasPrefix("|") && !$0.hasPrefix("#") && !$0.hasPrefix("RES|")
                && ($0.contains("E5") || $0.contains("ANE model load"))
        }
        print("- komunikaty E5RT/ANE na stdout potomka N=\(n): \(e5Stdout.count)" + (e5Stdout.isEmpty ? "" : " — np. `\(e5Stdout[0].prefix(200))`"))
        print("")
        Thread.sleep(forTimeInterval: 2)
    }
    return res
}

func printSummary(_ res: [String]) {
    print("### Zestawienie (mediany per predict; TFLOPS z mediany wall)")
    print("")
    print("| N | gate_up wall [ms] | gate_up CPU [ms] | gate_up TFLOPS | down wall [ms] | down CPU [ms] | down TFLOPS | runda N predict [ms] | CPU/wall (okno) | E5 bundle | pierwsze E5 przy modelu | aned RSS [MiB] |")
    print("|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|")
    for r in res {
        let f = r.split(separator: "|", omittingEmptySubsequences: false).map(String.init)
        guard f.count >= 4 else { continue }
        if f[3] != "ok" { print("| \(f[2]) | błąd: \(f.dropFirst(4).joined(separator: " ")) |"); continue }
        var gu = ["—", "—", "—"], dn = ["—", "—", "—"]
        for k in f[4].split(separator: ";") {
            let parts = k.split(separator: ":").map(String.init)
            if parts.count == 4 {
                if parts[0] == "gate_up" { gu = Array(parts[1...]) } else { dn = Array(parts[1...]) }
            }
        }
        func val(_ key: String) -> String {
            for x in f.dropFirst(5) where x.hasPrefix(key + "=") { return String(x.dropFirst(key.count + 1)) }
            return "—"
        }
        print("| \(f[2]) | \(gu[0]) | \(gu[1]) | \(gu[2]) | \(dn[0]) | \(dn[1]) | \(dn[2]) | \(val("round")) | \(val("cpushare")) | \(val("e5")) | \(val("first")) | \(val("aned")) |")
    }
    print("")
}

// ================================================================== main

let nsList = (opts["ns"] ?? defaultNs.map(String.init).joined(separator: ",")).split(separator: ",").compactMap { Int($0) }

switch mode {
case "run":
    startBallast()
    if let f = opts["stderr"] { redirectStderr(to: URL(fileURLWithPath: f)) }
    else { redirectStderr(to: outDir.appendingPathComponent("stderr_run_\(source)_\(fnName)_\(unitsOpt)_n\(nModels).txt")) }
    runOnce(n: nModels, label: "run")
    if let f = stderrFile {
        let s = (try? String(contentsOf: f, encoding: .utf8)) ?? ""
        let lines = s.split(separator: "\n").map(String.init)
        let uniq = Array(Set(lines.filter { $0.contains("E5") || $0.contains("ANE") || $0.contains("Neural") })).prefix(4)
        print("- stderr: \(lines.count) linii w `\(f.path)`" + (uniq.isEmpty ? "" : "; przykładowe: " + uniq.map { "`\($0.prefix(220))`" }.joined(separator: " / ")))
        print("")
    }
case "sweep":
    print("# EKS-A10 — wiele modeli ANE naraz: fn=\(fnName), units=\(unitsOpt), źródło=\(source), N ∈ \(nsList), każde N w NOWYM procesie")
    print("")
    print("maszyna: \(ProcessInfo.processInfo.operatingSystemVersionString), stan termiczny \(thermalState()), katalog `\(modelsDir.path)`")
    print("")
    if source == "copies" {
        // kopie (klony APFS) pliku L00_gate_up — czy problem to liczba modeli, czy łączny rozmiar wag
        let dir = outDir.appendingPathComponent("copies")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        for i in 0..<(nsList.max() ?? 80) {
            let dst = dir.appendingPathComponent(String(format: "L00_gate_up_%02d.mlmodelc", i))
            if !FileManager.default.fileExists(atPath: dst.path) {
                _ = shell("/bin/cp", ["-Rc", modelsDir.appendingPathComponent("L00_gate_up.mlmodelc").path, dst.path])
            }
        }
        print("- kopie: \(nsList.max() ?? 80) klonów APFS L00_gate_up.mlmodelc w `\(dir.path)`")
        print("")
    }
    printSummary(sweep(ns: nsList))
case "sweep-inproc":
    // kontrola zwalniania: te same N kolejno w JEDNYM procesie, modele zwalniane między N
    redirectStderr(to: outDir.appendingPathComponent("stderr_inproc_\(source)_\(fnName)_\(unitsOpt).txt"))
    print("# EKS-A10 — wiele modeli ANE naraz, N ∈ \(nsList) kolejno w JEDNYM procesie (zwalnianie między N)")
    print("")
    var res: [String] = []
    for n in nsList {
        autoreleasepool { runOnce(n: n, label: "inproc") }
        Thread.sleep(forTimeInterval: 2)
        let m = SysMem.now()
        let e = countE5()
        print("- po zwolnieniu N=\(n): \(m.text); E5 bundle łącznie ×\(e.bundle)")
        print("")
        res.append("RES|inproc|\(n)|ok|…")   // wiersze RES pochodzą z runOnce (wypisane wyżej)
    }
default:
    print("nieznany tryb \(mode)")
    exit(2)
}

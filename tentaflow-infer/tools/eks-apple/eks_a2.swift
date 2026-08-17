// ===== File: eks_a2.swift — EKS-A2: simdgroup_matrix instruction throughput =====
//
// Closes the last experiment blocking the Apple prefill target. Until this runs,
// that target rests on scaling 14.8 TFLOP/s measured on an M4 Max by GPU core
// count, which is a derivation, not a measurement.
//
// Method: N independent accumulator chains fed by simdgroup_multiply_accumulate
// on register-resident fragments. One chain measures instruction latency, not
// throughput, so the accumulator count is swept and the plateau is the answer.
// No memory traffic in the loop: this is the arithmetic ceiling, and the gap
// between it and a real GEMM is a separate measurement that belongs to NA2.
//
// Build and run: tools/eks-apple/run.sh a2

import Foundation
import Metal

let warmupIters = 300
let sampleRuns = 5
let mmaIterations: UInt32 = 200_000
let threadsPerGroup = 256          // 8 simdgroups of 32 lanes

struct Variant {
    let id: String
    let input: String              // MSL element type of A and B
    let acc: String                // MSL element type of the accumulator
}

let variants = [
    Variant(id: "f16 → f16", input: "half", acc: "half"),
    Variant(id: "f16 → f32", input: "half", acc: "float"),
    Variant(id: "bf16 → f32", input: "bfloat", acc: "float"),
]

/// One kernel per (variant, accumulator count). The count must be a compile
/// time constant or the chains cannot live in registers.
func mmaKernel(_ v: Variant, acc: Int) -> String {
    var decls = ""
    var body = ""
    var stores = ""
    for i in 0..<acc {
        decls += "    simdgroup_matrix<\(v.acc), 8, 8> c\(i) = simdgroup_matrix<\(v.acc), 8, 8>(0);\n"
        body += "        simdgroup_multiply_accumulate(c\(i), a, b, c\(i));\n"
        stores += "    simdgroup_store(c\(i), out + \(i) * 64 + sg * \(acc) * 64, 8);\n"
    }
    return """
    #include <metal_stdlib>
    #include <metal_simdgroup_matrix>
    using namespace metal;

    kernel void mma_rate(device \(v.acc)* out [[buffer(0)]],
                         constant uint& iters [[buffer(1)]],
                         uint sg [[simdgroup_index_in_threadgroup]],
                         uint tgid [[threadgroup_position_in_grid]]) {
        simdgroup_matrix<\(v.input), 8, 8> a = simdgroup_matrix<\(v.input), 8, 8>(1);
        simdgroup_matrix<\(v.input), 8, 8> b = simdgroup_matrix<\(v.input), 8, 8>(1);
    \(decls)
        for (uint i = 0; i < iters; ++i) {
    \(body)    }
        // The store is what keeps the chains alive; the address depends on the
        // threadgroup so nothing can be hoisted or shared between groups.
        if (tgid == 0) {
    \(stores)    }
    }
    """
}

/// Scalar FMA rate on the same ALUs. On M1..M4 the matrix instruction runs on
/// the shader cores with no dedicated datapath, so the honest question is not
/// "how fast is simdgroup_matrix" but "does it beat plain arithmetic at all".
/// Measuring both here answers it on this machine instead of citing someone
/// else's part.
func fmaKernel(acc: Int) -> String {
    var decls = ""
    var body = ""
    var sum = "c0"
    for i in 0..<acc {
        decls += "    float4 c\(i) = float4(\(i) + 1);\n"
        // Rotating dependency: each accumulator consumes the next one. Without
        // it `c = fma(a, b, c)` with loop invariant operands is a linear
        // recurrence the compiler closes into `c0 + n*a*b`, the loop vanishes,
        // and the harness reports a rate two orders of magnitude above the part.
        body += "        c\(i) = fma(c\((i + 1) % acc), b, c\(i));\n"
        if i > 0 { sum += " + c\(i)" }
    }
    return """
    #include <metal_stdlib>
    using namespace metal;

    kernel void fma_rate(device float* out [[buffer(0)]],
                         constant uint& iters [[buffer(1)]],
                         uint gid [[thread_position_in_grid]]) {
        // Operand zalezny od watku: bez tego arytmetyka jest jednakowa dla
        // wszystkich lane'ow, kompilator przenosi ja na sciezke skalarna
        // i harness raportuje rzad wielkosci za duzo. Zmierzone: 409 wobec
        // realnych 2,7 TFLOPS.
        float4 b = float4(0.999999f + float(gid) * 1e-12f);
    \(decls)
        for (uint i = 0; i < iters; ++i) {
    \(body)    }
        float4 s = \(sum);
        if (s.x + s.y + s.z + s.w == 12345.678f) { out[gid] = 1.0f; }
    }
    """
}

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

guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else {
    FileHandle.standardError.write("brak urządzenia Metal\n".data(using: .utf8)!)
    exit(1)
}

print("# EKS-A2 — przepustowość simdgroup_matrix")
print("urządzenie: \(device.name), stan termiczny: \(thermalState())")
print("")
print("Jedna instrukcja `simdgroup_multiply_accumulate` na kafle 8×8×8 to 1024 operacje")
print("zmiennoprzecinkowe. Pętla nie dotyka pamięci — mierzy sufit arytmetyczny.")
print("")
print("| wariant | akumulatory | grup roboczych | mediana [ms] | IQR | ważny | TFLOPS |")
print("|---|--:|--:|--:|--:|---|--:|")

let accCounts = [1, 2, 4, 8, 16]
let groupCounts = [40, 160, 640]

guard let out = device.makeBuffer(length: 16 * 64 * 8 * 4, options: .storageModeShared) else {
    exit(1)
}

var best: (Double, String, Int, Int) = (0, "", 0, 0)
var perVariantBest: [String: Double] = [:]

for v in variants {
    for acc in accCounts {
        var library: MTLLibrary?
        do {
            library = try device.makeLibrary(source: mmaKernel(v, acc: acc), options: nil)
        } catch {
            let msg = "\(error)".replacingOccurrences(of: "\n", with: " ").prefix(90)
            print("| \(v.id) | \(acc) | — | kompilacja nieudana | | NIE | \(msg) |")
            break
        }
        guard let fn = library?.makeFunction(name: "mma_rate"),
              let pipe = try? device.makeComputePipelineState(function: fn)
        else { continue }

        for groups in groupCounts {
            var iters = mmaIterations
            func onePass() -> Double {
                let t0 = nowNs()
                guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder()
                else { return 0 }
                enc.setComputePipelineState(pipe)
                enc.setBuffer(out, offset: 0, index: 0)
                enc.setBytes(&iters, length: 4, index: 1)
                enc.dispatchThreadgroups(
                    MTLSize(width: groups, height: 1, depth: 1),
                    threadsPerThreadgroup: MTLSize(width: threadsPerGroup, height: 1, depth: 1))
                enc.endEncoding()
                cb.commit()
                cb.waitUntilCompleted()
                return Double(nowNs() - t0) / 1e6
            }

            for _ in 0..<3 { _ = onePass() }
            var samples: [Double] = []
            for _ in 0..<sampleRuns { samples.append(onePass()) }
            samples.removeFirst()

            let ms = median(samples)
            guard ms > 0 else { continue }
            let simdgroups = threadsPerGroup / 32
            let mmas = Double(iters) * Double(acc) * Double(simdgroups) * Double(groups)
            let tflops = mmas * 1024.0 / (ms / 1000.0) / 1e12
            let spread = iqr(samples) / ms * 100
            let valid = spread <= 3.0
            if valid && tflops > best.0 { best = (tflops, v.id, acc, groups) }
            if valid { perVariantBest[v.id] = max(perVariantBest[v.id] ?? 0, tflops) }
            print(String(format: "| %@ | %d | %d | %.2f | %.1f%% | %@ | **%.2f** |",
                         v.id, acc, groups, ms, spread, valid ? "tak" : "NIE", tflops))
        }
    }
}

print("")
for v in variants {
    if let t = perVariantBest[v.id] {
        print(String(format: "- %@: **%.2f TFLOPS**", v.id, t))
    } else {
        print("- \(v.id): brak ważnego pomiaru")
    }
}
print("")
print(String(format: "Najlepszy WAŻNY wynik: **%.2f TFLOPS** (%@, %d akumulatorów, %d grup).",
             best.0, best.1, best.2, best.3))
print("")

// ------------------------------------------- przełożenie na prefill 7B 4-bit

// Bielik-Minitron-7B: 40 warstw, hidden 4096, inter 11264, GQA 8 głowic KV.
// Praca prefillu na token to 2 * liczba_parametrów operacji.
let params = 7.5e9
let flopsPerToken = 2.0 * params
print("### Przełożenie na prefill (7,5 mld parametrów, 2·P operacji na token)")
print("")
print("| wariant | TFLOPS | sufit prefillu [tok/s] | 70% sufitu |")
print("|---|--:|--:|--:|")
for v in variants {
    guard let t = perVariantBest[v.id] else { continue }
    let ceiling = t * 1e12 / flopsPerToken
    print(String(format: "| %@ | %.2f | **%.0f** | %.0f |", v.id, t, ceiling, ceiling * 0.7))
}
// ------------------------------------------------ FP32 FMA dla porównania

print("### Porównanie: zwykłe FMA na FP32")
print("")
print("| akumulatory | grup | mediana [ms] | IQR | ważny | TFLOPS |")
print("|---|--:|--:|--:|---|--:|")

var bestFma = 0.0
for acc in [2, 4, 8, 16] {
    guard let lib = try? device.makeLibrary(source: fmaKernel(acc: acc), options: nil),
          let fn = lib.makeFunction(name: "fma_rate"),
          let pipe = try? device.makeComputePipelineState(function: fn)
    else { continue }
    for groups in [160, 640] {
        var iters = mmaIterations
        func onePass() -> Double {
            let t0 = nowNs()
            guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder()
            else { return 0 }
            enc.setComputePipelineState(pipe)
            enc.setBuffer(out, offset: 0, index: 0)
            enc.setBytes(&iters, length: 4, index: 1)
            enc.dispatchThreadgroups(
                MTLSize(width: groups, height: 1, depth: 1),
                threadsPerThreadgroup: MTLSize(width: threadsPerGroup, height: 1, depth: 1))
            enc.endEncoding()
            cb.commit()
            cb.waitUntilCompleted()
            return Double(nowNs() - t0) / 1e6
        }
        for _ in 0..<3 { _ = onePass() }
        var samples: [Double] = []
        for _ in 0..<sampleRuns { samples.append(onePass()) }
        samples.removeFirst()
        let ms = median(samples)
        guard ms > 0 else { continue }
        // 4 składowe wektora, 2 operacje na FMA, wszystkie wątki siatki.
        let flops = Double(iters) * Double(acc) * 4.0 * 2.0
            * Double(threadsPerGroup) * Double(groups)
        let tflops = flops / (ms / 1000.0) / 1e12
        let spread = iqr(samples) / ms * 100
        let valid = spread <= 3.0
        if valid { bestFma = max(bestFma, tflops) }
        print(String(format: "| %d | %d | %.2f | %.1f%% | %@ | **%.2f** |",
                     acc, groups, ms, spread, valid ? "tak" : "NIE", tflops))
    }
}

print("")
print(String(format: "FP32 FMA: **%.2f TFLOPS**. Instrukcja macierzowa daje **%.2f×** tego.",
             bestFma, bestFma > 0 ? best.0 / bestFma : 0))
print("")
print("Stan termiczny na końcu: \(thermalState())")

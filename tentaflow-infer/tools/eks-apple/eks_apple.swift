// ===== File: eks_apple.swift — EKS-A1/A3: memory ceiling and dispatch cost on Apple GPUs =====
//
// EKS-A1 answers what fraction of the catalogue memory bandwidth a streaming
// kernel actually delivers, with an ILP sweep, because a measurement taken with
// too few accumulation chains is latency bound and reports about half the
// ceiling. EKS-A3 answers what one dispatch costs inside a command buffer,
// what one command buffer costs, and what a host round trip costs — which is
// what decides whether kernel fusion is a lever on this platform at all.
//
// Build and run: tools/eks-apple/run.sh

import Foundation
import Metal

// ---------------------------------------------------------------- parameters

let warmupIters = 300          // idle GPUs sit at a low clock; short runs never boost
let sampleRuns = 5             // first run discarded, median of the rest reported
let streamBytes = 2 << 30      // 2 GiB working set: far past any cache on this part
let threadsPerGroup = 256
let dispatchCount = 1000

// ------------------------------------------------------------------ MSL source

/// One streaming kernel per accumulator count. The count has to be a compile
/// time constant: passed as an argument it would live in a register the
/// compiler cannot unroll against, and the sweep would measure nothing.
func streamKernel(acc: Int) -> String {
    var body = ""
    for i in 0..<acc {
        body += "        a\(i) += src[i + \(i)u * gsize];\n"
    }
    var decls = ""
    for i in 0..<acc {
        decls += "    float4 a\(i) = float4(0.0f);\n"
    }
    var sum = "a0"
    for i in 1..<acc {
        sum += " + a\(i)"
    }
    return """
    kernel void stream_acc\(acc)(device const float4* src [[buffer(0)]],
                                 device float* out [[buffer(1)]],
                                 constant uint& n4 [[buffer(2)]],
                                 uint gid [[thread_position_in_grid]],
                                 uint gsize [[threads_per_grid]]) {
    \(decls)
        for (uint i = gid; i + \(acc - 1)u * gsize < n4; i += \(acc)u * gsize) {
    \(body)    }
        float4 s = \(sum);
        // The store keeps the loads alive; without it the whole loop folds away.
        if (s.x + s.y + s.z + s.w == 12345.678f) { out[gid] = 1.0f; }
    }
    """
}

let dispatchSource = """
kernel void nop(uint gid [[thread_position_in_grid]]) { }

// A dispatch that carries a real data dependency, so the cost is not measured
// on work the driver could reorder freely.
kernel void rmw(device float* buf [[buffer(0)]], uint gid [[thread_position_in_grid]]) {
    buf[gid] = buf[gid] * 1.0000001f + 1.0f;
}
"""

// ------------------------------------------------------------------ utilities

func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

func median(_ values: [Double]) -> Double {
    let s = values.sorted()
    if s.isEmpty { return 0 }
    return s.count % 2 == 1 ? s[s.count / 2] : (s[s.count / 2 - 1] + s[s.count / 2]) / 2
}

func iqr(_ values: [Double]) -> Double {
    let s = values.sorted()
    if s.count < 4 { return 0 }
    return s[(s.count * 3) / 4] - s[s.count / 4]
}

/// Every measurement carries the thermal state, for the same reason the AMD
/// protocol carries the memory clock DPM index: a run taken while throttled is
/// not comparable and must not silently become a baseline.
func thermalState() -> String {
    switch ProcessInfo.processInfo.thermalState {
    case .nominal: return "nominal"
    case .fair: return "fair"
    case .serious: return "serious"
    case .critical: return "critical"
    @unknown default: return "unknown"
    }
}

// ------------------------------------------------------------------- harness

guard let device = MTLCreateSystemDefaultDevice(),
      let queue = device.makeCommandQueue()
else {
    FileHandle.standardError.write("brak urządzenia Metal\n".data(using: .utf8)!)
    exit(1)
}

print("# EKS-A1/A3 — Apple GPU")
print("urządzenie: \(device.name)")
print("pamięć unified: \(device.hasUnifiedMemory)")
print("zalecany budżet roboczy: \(device.recommendedMaxWorkingSetSize / (1 << 20)) MiB")
print("max threadgroup memory: \(device.maxThreadgroupMemoryLength) B")
print("stan termiczny na starcie: \(thermalState())")
print("")

// -------------------------------------------------------------------- EKS-A1

print("## EKS-A1 — przepustowość pamięci, sweep ILP")
print("")
print("| akumulatory | grup roboczych | mediana [ms] | IQR/mediana | ważny | GB/s | % z 120 GB/s |")
print("|---|--:|--:|--:|---|--:|--:|")

let accCounts = [1, 2, 4, 8, 16]
let groupCounts = [256, 1024, 4096]
var bestBandwidth = 0.0
var bestAcc = 0
var bestGroups = 0

let elemCount = streamBytes / MemoryLayout<Float>.size
guard let src = device.makeBuffer(length: streamBytes, options: .storageModeShared),
      let out = device.makeBuffer(length: threadsPerGroup * groupCounts.max()! * 4, options: .storageModeShared)
else {
    FileHandle.standardError.write("alokacja nie powiodła się\n".data(using: .utf8)!)
    exit(1)
}
// Real values, not zeros: a zero page can take a different path in the memory
// subsystem and the number stops describing model weights.
let srcPtr = src.contents().bindMemory(to: Float.self, capacity: elemCount)
for i in 0..<elemCount { srcPtr[i] = Float(i & 1023) * 0.001 }

for acc in accCounts {
  for groups in groupCounts {
    let library: MTLLibrary
    do {
        library = try device.makeLibrary(source: streamKernel(acc: acc), options: nil)
    } catch {
        print("| \(acc) | kompilacja nieudana: \(error) | | | |")
        continue
    }
    guard let fn = library.makeFunction(name: "stream_acc\(acc)"),
          let pipeline = try? device.makeComputePipelineState(function: fn)
    else { continue }

    let gsize = threadsPerGroup * groups
    let chunk = gsize * acc
    let n4Total = elemCount / 4
    let n4 = (n4Total / chunk) * chunk
    var n4u = UInt32(n4)
    let bytes = Double(n4) * 16.0

    func onePass() -> Double {
        let t0 = nowNs()
        guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else {
            return 0
        }
        enc.setComputePipelineState(pipeline)
        enc.setBuffer(src, offset: 0, index: 0)
        enc.setBuffer(out, offset: 0, index: 1)
        enc.setBytes(&n4u, length: 4, index: 2)
        enc.dispatchThreadgroups(MTLSize(width: groups, height: 1, depth: 1),
                                 threadsPerThreadgroup: MTLSize(width: threadsPerGroup, height: 1, depth: 1))
        enc.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
        return Double(nowNs() - t0) / 1e6
    }

    // Warm-up on the same shape as the measurement.
    for _ in 0..<3 { _ = onePass() }
    var samples: [Double] = []
    for _ in 0..<sampleRuns { samples.append(onePass()) }
    samples.removeFirst()

    let ms = median(samples)
    let gbs = bytes / (ms / 1000.0) / 1e9
    let spread = iqr(samples) / ms * 100
    let valid = spread <= 3.0
    if gbs > bestBandwidth && valid {
        bestBandwidth = gbs
        bestAcc = acc
        bestGroups = groups
    }
    print(String(format: "| %d | %d | %.3f | %.1f%% | %@ | **%.1f** | %.1f%% |",
                 acc, groups, ms, spread, valid ? "tak" : "NIE", gbs, gbs / 120.0 * 100))
  }
}

print("")
print(String(format: "Najlepszy WAŻNY wynik: **%.1f GB/s** przy %d akumulatorach i %d grupach (%.1f%% z katalogowych 120 GB/s).",
             bestBandwidth, bestAcc, bestGroups, bestBandwidth / 120.0 * 100))
print("")

// -------------------------------------------------------------------- EKS-A3

print("## EKS-A3 — koszt dyspozycji, command buffera i powrotu na hosta")
print("")

guard let dl = try? device.makeLibrary(source: dispatchSource, options: nil),
      let nopFn = dl.makeFunction(name: "nop"),
      let rmwFn = dl.makeFunction(name: "rmw"),
      let nopPipe = try? device.makeComputePipelineState(function: nopFn),
      let rmwPipe = try? device.makeComputePipelineState(function: rmwFn),
      let small = device.makeBuffer(length: threadsPerGroup * 4, options: .storageModeShared)
else {
    FileHandle.standardError.write("kompilacja kerneli dyspozycji nie powiodła się\n".data(using: .utf8)!)
    exit(1)
}

let one = MTLSize(width: 1, height: 1, depth: 1)
let tg = MTLSize(width: threadsPerGroup, height: 1, depth: 1)

func encode(_ enc: MTLComputeCommandEncoder, _ pipe: MTLComputePipelineState, dependent: Bool) {
    enc.setComputePipelineState(pipe)
    if dependent { enc.setBuffer(small, offset: 0, index: 0) }
    enc.dispatchThreadgroups(one, threadsPerThreadgroup: tg)
}

/// N dispatches inside a single command buffer: the per-dispatch cost that
/// fusion actually removes.
func dispatchesInOneBuffer(_ n: Int, dependent: Bool) -> Double {
    let pipe = dependent ? rmwPipe : nopPipe
    let t0 = nowNs()
    guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else { return 0 }
    for _ in 0..<n { encode(enc, pipe, dependent: dependent) }
    enc.endEncoding()
    cb.commit()
    cb.waitUntilCompleted()
    return Double(nowNs() - t0) / 1e3 / Double(n)
}

/// N dispatches, each in its own command buffer, all committed before waiting:
/// the cost of a command buffer without a host round trip.
func oneDispatchPerBuffer(_ n: Int) -> Double {
    let t0 = nowNs()
    var last: MTLCommandBuffer?
    for _ in 0..<n {
        guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else { continue }
        encode(enc, nopPipe, dependent: false)
        enc.endEncoding()
        cb.commit()
        last = cb
    }
    last?.waitUntilCompleted()
    return Double(nowNs() - t0) / 1e3 / Double(n)
}

/// Commit and wait for every step: the analogue of returning to the host once
/// per layer.
func hostRoundTrip(_ n: Int) -> Double {
    let t0 = nowNs()
    for _ in 0..<n {
        guard let cb = queue.makeCommandBuffer(), let enc = cb.makeComputeCommandEncoder() else { continue }
        encode(enc, nopPipe, dependent: false)
        enc.endEncoding()
        cb.commit()
        cb.waitUntilCompleted()
    }
    return Double(nowNs() - t0) / 1e3 / Double(n)
}

// Warm-up: an unwarmed empty kernel measures the clock ramp, not the dispatch.
for _ in 0..<warmupIters {
    _ = dispatchesInOneBuffer(10, dependent: false)
}

func report(_ label: String, runs: Int = sampleRuns, _ f: () -> Double) -> Double {
    var samples: [Double] = []
    for _ in 0..<runs { samples.append(f()) }
    samples.removeFirst()
    let m = median(samples)
    let spread = iqr(samples) / m * 100
    print(String(format: "| %@ | **%.3f** | %.1f%% | %@ |", label, m, spread, spread <= 3.0 ? "tak" : "NIE"))
    return m
}

print("| pomiar | µs na dyspozycję | IQR/mediana | ważny |")
print("|---|--:|--:|---|")
let inBuffer = report("dyspozycja w jednym command bufferze (pusty kernel)") {
    dispatchesInOneBuffer(dispatchCount, dependent: false)
}
let inBufferDep = report("dyspozycja w jednym command bufferze (zależność danych)") {
    dispatchesInOneBuffer(dispatchCount, dependent: true)
}
let perBuffer = report("jeden command buffer na dyspozycję, bez czekania") {
    oneDispatchPerBuffer(dispatchCount)
}
let roundTrip = report("commit + waitUntilCompleted na każdą dyspozycję", runs: 11) {
    hostRoundTrip(200)
}

print("")
print("Stan termiczny na końcu: \(thermalState())")
print("")
print("### Przeliczenie na krok dekodowania")
print("")
print("| ścieżka | dyspozycji na token | koszt narzutu na token |")
print("|---|--:|--:|")
for launches in [681, 200, 65] {
    print(String(format: "| %d dyspozycji w jednym buforze | %d | %.3f ms |",
                 launches, launches, Double(launches) * inBuffer / 1000.0))
}
print(String(format: "| 65 dyspozycji, osobne bufory | 65 | %.3f ms |", 65.0 * perBuffer / 1000.0))
print(String(format: "| 65 powrotów na hosta | 65 | %.3f ms |", 65.0 * roundTrip / 1000.0))
print("")
print(String(format: "Oszczędność na usuniętym punkcie synchronizacji wewnątrz bufora: **%.3f µs**.", inBuffer))
print(String(format: "Zależność danych podnosi koszt dyspozycji o %.1f%%.",
             (inBufferDep / inBuffer - 1.0) * 100))

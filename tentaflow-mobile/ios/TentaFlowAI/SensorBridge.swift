// =============================================================================
// Plik: SensorBridge.swift
// Opis: Natywne przechwytywanie czujników iPhone/iPad → kanoniczne próbki → Rust.
//       CoreMotion (akcelerometr+żyroskop → IMU, barometr), CoreLocation (GPS),
//       ARKit (sceneDepth → chmura punktów świata → LidarFrame). Każdą próbkę
//       kodujemy DOKŁADNIE w binarnym layoutcie z tentaflow-sdk-spec (little-endian)
//       i przekazujemy przez `tentaflow_mobile_push_sensor`. Logika fuzji (ESKF) i
//       mapy żyje w rdzeniu; uprawnienia per-czujnik addonu `phone` decydują, co
//       faktycznie trafia do silnika.
// =============================================================================

import ARKit
import CoreLocation
import CoreMotion
import Foundation

// Zgodne ze stałymi SENSOR_KIND_* w rdzeniu (services/mobile_sensors.rs).
private let KIND_IMU: Int32 = 1
private let KIND_GNSS: Int32 = 2
private let KIND_BARO: Int32 = 3
private let KIND_DEPTH: Int32 = 4
private let KIND_POSE: Int32 = 5
private let KIND_MAG: Int32 = 6

private let ACCEL_NOISE: Float = 0.02
private let GYRO_NOISE: Float = 0.002
private let BARO_NOISE: Float = 0.6
private let MAG_NOISE: Float = 1.5
private let GRAVITY: Double = 9.81

// ----- Mały koder little-endian (Data) -----

private struct LE {
    var data = Data()
    mutating func u8(_ v: UInt8) { data.append(v) }
    mutating func i32(_ v: Int32) { var x = v.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
    mutating func i64(_ v: Int64) { var x = v.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
    mutating func f32(_ v: Float) { var x = v.bitPattern.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
    mutating func f64(_ v: Double) { var x = v.bitPattern.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
}

private func push(_ kind: Int32, _ data: Data) {
    data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
        guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return }
        _ = tentaflow_mobile_push_sensor(kind, base, Int32(data.count))
    }
}

private func nowUs() -> Int64 { Int64(ProcessInfo.processInfo.systemUptime * 1_000_000) }

final class SensorBridge: NSObject, CLLocationManagerDelegate, ARSessionDelegate {
    private let motion = CMMotionManager()
    private let altimeter = CMAltimeter()
    private let location = CLLocationManager()
    private var arSession: ARSession?
    private var depthSeq: Int32 = 0
    private let motionQueue = OperationQueue()

    func start(imu: Bool, baro: Bool, gnss: Bool, depth: Bool) {
        if imu { startImu() }
        if baro { startBaro() }
        if gnss { startGnss() }
        if depth { startDepth() }
    }

    func stop() {
        motion.stopGyroUpdates()
        motion.stopAccelerometerUpdates()
        motion.stopMagnetometerUpdates()
        altimeter.stopRelativeAltitudeUpdates()
        location.stopUpdatingLocation()
        arSession?.pause()
        arSession = nil
        tentaflow_mobile_clear_sensors()
    }

    // ----- IMU (akcelerometr + żyroskop) -----

    private var latestGyro: CMRotationRate?

    private func startImu() {
        guard motion.isAccelerometerAvailable, motion.isGyroAvailable else { return }
        motion.accelerometerUpdateInterval = 0.01 // 100 Hz
        motion.gyroUpdateInterval = 0.01
        motion.startGyroUpdates(to: motionQueue) { [weak self] data, _ in
            if let d = data { self?.latestGyro = d.rotationRate }
        }
        // Magnetometer: a heading aid riding the IMU grant.
        if motion.isMagnetometerAvailable {
            motion.magnetometerUpdateInterval = 0.05
            motion.startMagnetometerUpdates(to: motionQueue) { data, _ in
                guard let m = data else { return }
                var b = LE()
                b.u8(1); b.u8(0); b.u8(0); b.u8(0)
                b.i64(nowUs())
                b.f32(Float(m.magneticField.x)); b.f32(Float(m.magneticField.y)); b.f32(Float(m.magneticField.z))
                b.f32(MAG_NOISE)
                push(KIND_MAG, b.data)
            }
        }
        motion.startAccelerometerUpdates(to: motionQueue) { [weak self] data, _ in
            guard let self = self, let a = data, let g = self.latestGyro else { return }
            var b = LE()
            b.u8(1); b.u8(0); b.u8(0); b.u8(0)         // version, flags, 2× reserved
            b.i64(nowUs())
            // CoreMotion: G (z grawitacją). → m/s². Oś standardowa urządzenia.
            b.f32(Float(a.acceleration.x * GRAVITY))
            b.f32(Float(a.acceleration.y * GRAVITY))
            b.f32(Float(a.acceleration.z * GRAVITY))
            b.f32(Float(g.x)); b.f32(Float(g.y)); b.f32(Float(g.z)) // rad/s
            b.f32(ACCEL_NOISE); b.f32(GYRO_NOISE)
            push(KIND_IMU, b.data)
        }
    }

    // ----- Barometr -----

    private func startBaro() {
        guard CMAltimeter.isRelativeAltitudeAvailable() else { return }
        altimeter.startRelativeAltitudeUpdates(to: motionQueue) { data, _ in
            guard let d = data else { return }
            var b = LE()
            b.u8(1); b.u8(0); b.u8(0); b.u8(0)
            b.i64(nowUs())
            b.f32(Float(truncating: d.pressure) * 1000.0) // kPa → Pa
            b.f32(Float(truncating: d.relativeAltitude))   // m
            b.f32(BARO_NOISE)
            push(KIND_BARO, b.data)
        }
    }

    // ----- GPS -----

    private func startGnss() {
        location.delegate = self
        location.desiredAccuracy = kCLLocationAccuracyBest
        location.requestWhenInUseAuthorization()
        location.startUpdatingLocation()
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let loc = locations.last else { return }
        var b = LE()
        let hasVel = loc.speed >= 0
        b.u8(1); b.u8(hasVel ? 1 : 0); b.u8(0); b.u8(0)
        b.i64(nowUs())
        b.f64(loc.coordinate.latitude); b.f64(loc.coordinate.longitude); b.f64(loc.altitude)
        b.f32(Float(loc.horizontalAccuracy > 0 ? loc.horizontalAccuracy : 10))
        b.f32(Float(loc.verticalAccuracy > 0 ? loc.verticalAccuracy : 20))
        if hasVel {
            let course = loc.course >= 0 ? loc.course * .pi / 180 : 0
            let speed = loc.speed
            b.f32(Float(speed * sin(course)))  // East
            b.f32(Float(speed * cos(course)))  // North
            b.f32(0)                           // Up
            b.f32(Float(loc.speedAccuracy > 0 ? loc.speedAccuracy : 0.5))
        } else {
            b.f32(0); b.f32(0); b.f32(0); b.f32(0)
        }
        push(KIND_GNSS, b.data)
    }

    // ----- Głębia (ARKit sceneDepth → chmura punktów świata → LidarFrame) -----
    // UWAGA: mapa buduje się w układzie świata ARKit; georeferencja (WGS84) przez
    // auto-geo-anchor z GNSS — wyrównanie obu układów to krok na żywym sprzęcie.

    private let depthStride = 8
    private let resolution: Float = 0.05
    private let maxDepth: Float = 8.0

    private func startDepth() {
        guard ARWorldTrackingConfiguration.supportsFrameSemantics(.sceneDepth) else { return }
        let cfg = ARWorldTrackingConfiguration()
        cfg.frameSemantics = .sceneDepth
        let s = ARSession()
        s.delegate = self
        s.run(cfg)
        arSession = s
    }

    func session(_ session: ARSession, didUpdate frame: ARFrame) {
        // AR device pose (ARKit world frame, Y-up) → engine Z-up: pos [x,-z,y],
        // orientation rotated +90° about X. Drives marker + map frame + AR↔ENU align.
        let tf = frame.camera.transform
        let q = simd_quatf(angle: .pi / 2, axis: SIMD3<Float>(1, 0, 0)) * simd_quatf(tf)
        let p = tf.columns.3
        var pb = LE()
        pb.u8(1); pb.u8(0); pb.u8(0); pb.u8(0)
        pb.i64(nowUs())
        pb.f32(p.x); pb.f32(-p.z); pb.f32(p.y)
        pb.f32(q.vector.x); pb.f32(q.vector.y); pb.f32(q.vector.z); pb.f32(q.vector.w)
        push(KIND_POSE, pb.data)

        guard let depth = frame.sceneDepth else { return }
        let map = depth.depthMap
        CVPixelBufferLockBaseAddress(map, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(map, .readOnly) }
        let w = CVPixelBufferGetWidth(map)
        let h = CVPixelBufferGetHeight(map)
        guard let base = CVPixelBufferGetBaseAddress(map) else { return }
        let rowBytes = CVPixelBufferGetBytesPerRow(map)
        let ptr = base.assumingMemoryBound(to: Float32.self)
        let rowFloats = rowBytes / MemoryLayout<Float32>.size

        // Intrinsics skalowane do rozdzielczości mapy głębi.
        let intr = frame.camera.intrinsics
        let imgRes = frame.camera.imageResolution
        let sx = Float(w) / Float(imgRes.width)
        let sy = Float(h) / Float(imgRes.height)
        let fx = intr[0][0] * sx, fy = intr[1][1] * sy
        let cx = intr[2][0] * sx, cy = intr[2][1] * sy
        let view = frame.camera.transform // camera → world (ARKit)

        var bodyLE = LE()
        var count: Int32 = 0
        var v = 0
        while v < h {
            var u = 0
            while u < w {
                let z = ptr[v * rowFloats + u]
                if z > 0.1 && z < maxDepth {
                    // ARKit kamera: +X right, +Y up, patrzy w −Z.
                    let xc = (Float(u) - cx) * z / fx
                    let yc = -(Float(v) - cy) * z / fy
                    let zc = -z
                    let p = view * simd_float4(xc, yc, zc, 1)
                    // Y-up (ARKit) → engine Z-up: [x, -z, y].
                    bodyLE.f32(p.x); bodyLE.f32(-p.z); bodyLE.f32(p.y)
                    count += 1
                }
                u += depthStride
            }
            v += depthStride
        }
        if count == 0 { return }
        // Nagłówek LidarFrame (44 B): version=2, layout XYZ=3.
        var b = LE()
        b.u8(2); b.u8(3); b.u8(0); b.u8(0)
        b.i32(count)
        b.i32(depthSeq); depthSeq += 1
        b.i64(nowUs())
        b.i64(0)                 // host_send_us
        b.f32(resolution)
        b.f32(0); b.f32(0); b.f32(0) // origin
        b.data.append(bodyLE.data)
        push(KIND_DEPTH, b.data)
    }
}

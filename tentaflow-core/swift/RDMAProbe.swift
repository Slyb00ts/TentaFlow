// =============================================================================
// Plik: swift/RDMAProbe.swift
// Opis: macOS RDMA probe via Network.framework NWConnection.
//       Rejestruje callbacki FFI przy starcie (tentaflow_register_rdma_*).
//       Wymaga macOS Tahoe 26.2+ dla NWProtocolRDMA.
//       Kompilacja: swiftc -emit-library -o librdma_probe.dylib RDMAProbe.swift
//                   -framework Network -framework Foundation
//       Albo przez Xcode w tentaflow-desktop/macos.
// =============================================================================

import Foundation
import Network

// MARK: - FFI function declarations (defined in Rust, called from Swift)

@_silgen_name("tentaflow_register_rdma_probe_server")
func tentaflow_register_rdma_probe_server(
    _ f: @convention(c) (
        UnsafePointer<CChar>,   // bind_ip
        UnsafePointer<UInt8>,   // nonce
        UInt32,                 // nonce_len
        UInt32,                 // duration_ms
        @convention(c) (UInt64, UInt64, Double, UnsafeMutableRawPointer?) -> Void, // result_callback
        UnsafeMutableRawPointer? // callback_ctx
    ) -> Int32
)

@_silgen_name("tentaflow_register_rdma_probe_client")
func tentaflow_register_rdma_probe_client(
    _ f: @convention(c) (
        UnsafePointer<CChar>,   // target_ip
        UInt16,                 // target_port
        UnsafePointer<UInt8>,   // nonce
        UInt32,                 // nonce_len
        UInt32,                 // duration_ms
        @convention(c) (UInt64, UInt64, Double, UnsafeMutableRawPointer?) -> Void, // result_callback
        UnsafeMutableRawPointer? // callback_ctx
    ) -> Int32
)

@_silgen_name("tentaflow_register_rdma_available")
func tentaflow_register_rdma_available(
    _ f: @convention(c) () -> Bool
)

@_silgen_name("tentaflow_register_rdma_list_devices")
func tentaflow_register_rdma_list_devices(
    _ f: @convention(c) () -> UnsafeMutablePointer<CChar>?
)

// MARK: - Callback type alias

typealias RdmaResultCallback = @convention(c) (UInt64, UInt64, Double, UnsafeMutableRawPointer?) -> Void

// MARK: - Registration entry point (called from native app startup)

/// Register all RDMA probe callbacks with the Rust side.
/// Call this from AppDelegate or equivalent after tentaflow-core init.
@_cdecl("tentaflow_rdma_swift_init")
public func rdmaSwiftInit() {
    tentaflow_register_rdma_available(rdmaIsAvailable)
    tentaflow_register_rdma_probe_server(rdmaProbeServer)
    tentaflow_register_rdma_probe_client(rdmaProbeClient)
    tentaflow_register_rdma_list_devices(rdmaListDevices)
}

// MARK: - RDMA availability detection

/// Check if Thunderbolt 5 RDMA is available via Network.framework.
/// Returns true on macOS Tahoe 26.2+ where NWProtocolRDMA exists.
func rdmaIsAvailable() -> Bool {
    // Detect Thunderbolt 5 connections via system_profiler
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/sbin/system_profiler")
    process.arguments = ["SPThunderboltDataType", "-json"]

    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = Pipe()

    do {
        try process.run()
        process.waitUntilExit()

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        if let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
           let tbData = json["SPThunderboltDataType"] as? [[String: Any]] {
            // Thunderbolt 5 reports "USB4 Version" >= 2.0 or "Speed" containing "120"
            for device in tbData {
                if let speed = device["Speed"] as? String,
                   speed.contains("120") {
                    return true
                }
                if let linkSpeed = device["link_speed"] as? String,
                   linkSpeed.contains("120") {
                    return true
                }
            }
        }
    } catch {
        // system_profiler failed — assume no TB5
    }

    return false
}

// MARK: - Device listing

/// Return a JSON array of RDMA device names as a C string.
/// Caller (Rust side) is responsible for freeing the returned pointer.
func rdmaListDevices() -> UnsafeMutablePointer<CChar>? {
    var devices: [String] = []

    // Detect Thunderbolt interfaces via system_profiler
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/sbin/system_profiler")
    process.arguments = ["SPThunderboltDataType", "-json"]

    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = Pipe()

    do {
        try process.run()
        process.waitUntilExit()

        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        if let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
           let tbData = json["SPThunderboltDataType"] as? [[String: Any]] {
            for (index, device) in tbData.enumerated() {
                let name = device["_name"] as? String ?? "thunderbolt\(index)"
                devices.append(name)
            }
        }
    } catch {
        // Ignore errors
    }

    // Serialize to JSON and return as C string
    guard let jsonData = try? JSONSerialization.data(withJSONObject: devices),
          let jsonString = String(data: jsonData, encoding: .utf8) else {
        return nil
    }

    return strdup(jsonString)
}

// MARK: - Probe server

/// Start an RDMA probe server that listens for incoming connections.
/// Returns the bound port (>0) or error code (<0).
/// The result_callback is invoked when the probe measurement completes.
func rdmaProbeServer(
    bindIp: UnsafePointer<CChar>,
    nonce: UnsafePointer<UInt8>,
    nonceLen: UInt32,
    durationMs: UInt32,
    resultCallback: RdmaResultCallback,
    callbackCtx: UnsafeMutableRawPointer?
) -> Int32 {
    let ip = String(cString: bindIp)
    let nonceData = Data(bytes: nonce, count: Int(nonceLen))

    let params = NWParameters.tcp
    params.requiredInterfaceType = .other // Thunderbolt interfaces are .other

    let listener: NWListener
    do {
        listener = try NWListener(using: params, on: 0)
    } catch {
        return -1 // Listener creation failed
    }

    var actualPort: Int32 = -2
    let portSemaphore = DispatchSemaphore(value: 0)
    let completionSemaphore = DispatchSemaphore(value: 0)

    var totalBytes: UInt64 = 0
    let startTime = DispatchTime.now()
    let queue = DispatchQueue(label: "com.tentaflow.rdma.server", qos: .userInitiated)

    listener.stateUpdateHandler = { state in
        switch state {
        case .ready:
            if let port = listener.port {
                actualPort = Int32(port.rawValue)
            }
            portSemaphore.signal()
        case .failed:
            actualPort = -3
            portSemaphore.signal()
            completionSemaphore.signal()
        default:
            break
        }
    }

    listener.newConnectionHandler = { connection in
        connection.start(queue: queue)

        // Verify nonce first
        connection.receive(minimumIncompleteLength: Int(nonceLen), maximumLength: Int(nonceLen)) { data, _, _, error in
            guard let data = data, error == nil, data == nonceData else {
                connection.cancel()
                completionSemaphore.signal()
                return
            }

            // Receive data loop — measure throughput
            func receiveLoop() {
                connection.receive(minimumIncompleteLength: 1, maximumLength: 1024 * 1024) { data, _, isComplete, error in
                    if let data = data {
                        totalBytes += UInt64(data.count)
                    }
                    if isComplete || error != nil {
                        let elapsed = DispatchTime.now().uptimeNanoseconds - startTime.uptimeNanoseconds
                        let durationMs = UInt64(elapsed / 1_000_000)
                        let bwMbps = durationMs > 0 ? Double(totalBytes) * 8.0 / Double(durationMs) / 1000.0 : 0.0
                        resultCallback(totalBytes, durationMs, bwMbps, callbackCtx)
                        listener.cancel()
                        completionSemaphore.signal()
                    } else {
                        receiveLoop()
                    }
                }
            }
            receiveLoop()
        }
    }

    listener.start(queue: queue)

    // Wait for port assignment
    let portResult = portSemaphore.wait(timeout: .now() + .seconds(5))
    if portResult == .timedOut {
        listener.cancel()
        return -4 // Port assignment timeout
    }

    if actualPort < 0 {
        return actualPort
    }

    // Wait for probe completion (duration + 5s grace)
    let _ = completionSemaphore.wait(timeout: .now() + .milliseconds(Int(durationMs) + 5000))

    return actualPort
}

// MARK: - Probe client

/// Connect to an RDMA probe server and send data for measurement.
/// Returns 0 on success or error code (<0).
func rdmaProbeClient(
    targetIp: UnsafePointer<CChar>,
    targetPort: UInt16,
    nonce: UnsafePointer<UInt8>,
    nonceLen: UInt32,
    durationMs: UInt32,
    resultCallback: RdmaResultCallback,
    callbackCtx: UnsafeMutableRawPointer?
) -> Int32 {
    let ip = String(cString: targetIp)
    let nonceData = Data(bytes: nonce, count: Int(nonceLen))

    guard let port = NWEndpoint.Port(rawValue: targetPort) else {
        return -1 // Invalid port
    }

    let host = NWEndpoint.Host(ip)
    let params = NWParameters.tcp
    params.requiredInterfaceType = .other // Thunderbolt interfaces

    let connection = NWConnection(host: host, port: port, using: params)

    let semaphore = DispatchSemaphore(value: 0)
    var totalBytes: UInt64 = 0
    var errorCode: Int32 = 0
    let queue = DispatchQueue(label: "com.tentaflow.rdma.client", qos: .userInitiated)

    connection.stateUpdateHandler = { state in
        switch state {
        case .ready:
            // Send nonce for authentication
            connection.send(content: nonceData, completion: .contentProcessed({ sendError in
                if sendError != nil {
                    errorCode = -2
                    semaphore.signal()
                    return
                }

                // Send data for the specified duration
                let data = Data(repeating: 0xAB, count: 1024 * 1024) // 1 MB chunks
                let deadline = DispatchTime.now() + .milliseconds(Int(durationMs))

                func sendLoop() {
                    if DispatchTime.now() >= deadline {
                        // Measurement complete — report results
                        let elapsed = UInt64(durationMs)
                        let bwMbps = elapsed > 0 ? Double(totalBytes) * 8.0 / Double(elapsed) / 1000.0 : 0.0
                        resultCallback(totalBytes, elapsed, bwMbps, callbackCtx)
                        connection.cancel()
                        semaphore.signal()
                        return
                    }
                    connection.send(content: data, completion: .contentProcessed({ error in
                        if error == nil {
                            totalBytes += UInt64(data.count)
                        } else {
                            // Connection error — report partial results
                            let elapsed = UInt64(durationMs)
                            let bwMbps = elapsed > 0 ? Double(totalBytes) * 8.0 / Double(elapsed) / 1000.0 : 0.0
                            resultCallback(totalBytes, elapsed, bwMbps, callbackCtx)
                            connection.cancel()
                            semaphore.signal()
                            return
                        }
                        sendLoop()
                    }))
                }
                sendLoop()
            }))
        case .failed:
            errorCode = -3
            semaphore.signal()
        case .cancelled:
            break
        default:
            break
        }
    }

    connection.start(queue: queue)
    let _ = semaphore.wait(timeout: .now() + .milliseconds(Int(durationMs) + 5000))

    return errorCode
}

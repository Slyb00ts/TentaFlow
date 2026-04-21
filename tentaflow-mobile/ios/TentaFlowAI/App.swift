// =============================================================================
// Plik: App.swift
// Opis: Punkt wejscia iOS — uruchamia Rust core (serwisy w tle).
//       Serwer HTTPS na porcie 8090 dostepny z zewnatrz.
// =============================================================================

import SwiftUI

@main
struct TentaFlowAIApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        NSLog("[TentaFlow] didFinishLaunching — start")

        // Rejestruj Swift MLX callbacks PRZED startem Rust core
        NSLog("[TentaFlow] Rejestracja MLX callbacks...")
        MLXSwiftEngine.shared.registerWithRust()
        NSLog("[TentaFlow] MLX callbacks zarejestrowane")

        // Uruchom natywny mDNS discovery (Apple Network framework — dziala na iOS)
        let nodeId = UUID().uuidString
        NSLog("[TentaFlow] mDNS discovery start (nodeId=\(nodeId.prefix(8))...)")
        NativeDiscovery.shared.start(nodeId: nodeId, port: 8090)
        NSLog("[TentaFlow] mDNS discovery started")

        // Uruchom Rust core — serwisy startuja w osobnym watku, nie blokuje main thread
        NSLog("[TentaFlow] Wywolanie tentaflow_mobile_start()...")
        tentaflow_mobile_start()
        NSLog("[TentaFlow] tentaflow_mobile_start() zwrocilo — serwisy startuja w tle")

        // Sprawdz port 8090 po kilku sekundach
        DispatchQueue.global().asyncAfter(deadline: .now() + 5.0) {
            self.checkServerPort()
        }

        return true
    }

    private func checkServerPort() {
        let host = "127.0.0.1"
        let port: UInt16 = 8090
        var sin = sockaddr_in()
        sin.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        sin.sin_family = sa_family_t(AF_INET)
        sin.sin_port = port.bigEndian
        sin.sin_addr.s_addr = inet_addr(host)

        let sock = socket(AF_INET, SOCK_STREAM, 0)
        guard sock >= 0 else {
            NSLog("[TentaFlow] PORT CHECK: nie mozna utworzyc socketu")
            return
        }
        defer { close(sock) }

        let result = withUnsafePointer(to: &sin) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { addr in
                connect(sock, addr, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }

        if result == 0 {
            NSLog("[TentaFlow] PORT CHECK: port 8090 OTWARTY — serwer dziala")
        } else {
            NSLog("[TentaFlow] PORT CHECK: port 8090 ZAMKNIETY (errno=\(errno)) — serwer NIE nasłuchuje")
        }
    }

    func applicationDidEnterBackground(_ application: UIApplication) {
        tentaflow_on_pause()
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        tentaflow_on_resume()
    }

    func applicationDidReceiveMemoryWarning(_ application: UIApplication) {
        tentaflow_on_memory_warning()
    }
}

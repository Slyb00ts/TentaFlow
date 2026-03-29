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
        // Rejestruj Swift MLX callbacks PRZED startem Rust core
        MLXSwiftEngine.shared.registerWithRust()

        // Uruchom natywny mDNS discovery (Apple Network framework — dziala na iOS)
        let nodeId = UUID().uuidString
        NativeDiscovery.shared.start(nodeId: nodeId, port: 8090)

        // Uruchom Rust core — serwisy startuja w osobnym watku, nie blokuje main thread
        tentaflow_mobile_start()

        return true
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

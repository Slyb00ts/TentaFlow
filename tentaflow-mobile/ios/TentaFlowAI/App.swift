// =============================================================================
// Plik: App.swift
// Opis: Punkt wejscia iOS — uruchamia Rust core (serwisy w tle).
//       Serwer HTTPS na porcie 8090 dostepny z zewnatrz.
// =============================================================================

import SwiftUI
import WebKit

/// Notyfikacja emitowana przez AppDelegate gdy applicationWillEnterForeground.
/// ContentView/Coordinator nasluchuje i wykonuje recovery — WKWebView reload
/// albo fallback do pollServer jesli HTTPS 8090 zdechl podczas suspendu.
extension Notification.Name {
    static let tentaflowForegrounded = Notification.Name("TentaFlowForegrounded")
}

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
    /// Wspoldzielony pool procesow WebKit — ten sam dla warmup webview
    /// i dla realnego WKWebView w ContentView. Dzieki temu procesy
    /// GPU/WebContent/Networking spawnowane w warmup zostaja zreuzywane
    /// zamiast spawnowac sie od zera przy pierwszym load().
    static let sharedProcessPool = WKProcessPool()

    /// Warmup WKWebView — trzymany jako property zeby nie zostal zwolniony
    /// przed zakonczeniem inicjalizacji procesow WebKit.
    private var warmupWebView: WKWebView?

    /// Timestamp wejscia w tlo — sluzy do policzenia czasu w suspendzie
    /// przy powrocie (applicationWillEnterForeground).
    private var lastBackgroundTime: Date?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        NSLog("[TentaFlow] didFinishLaunching — start")

        // KROK 1: Rozpocznij warmup WKWebView natychmiast. Spawn procesow
        // GPU/WebContent/Networking (4.5s kazdy) odbywa sie rownolegle z
        // inicjalizacja Rust core zamiast blokowac glowny render po logowaniu.
        prewarmWebKit()

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

    /// Pre-warmup WKWebView — rzuca niewidoczny loadHTMLString zeby spawnowac
    /// procesy WebKit (GPU, WebContent, Networking) w tle, zanim user zobaczy
    /// ekran dashboardu. Bez tego pierwszy load() w ContentView czeka ~4.5s
    /// na kazdy z trzech procesow (widoczne jako zamrozony splash).
    private func prewarmWebKit() {
        NSLog("[TentaFlow] WebKit warmup start")
        let config = WKWebViewConfiguration()
        config.processPool = AppDelegate.sharedProcessPool
        // Uzywamy tego samego typu data store co ContentView (nonPersistent),
        // zeby procesy byly kompatybilne i dzielone.
        config.websiteDataStore = WKWebsiteDataStore.nonPersistent()

        let web = WKWebView(frame: .zero, configuration: config)
        // Minimalna strona — wystarczy zeby wymusic spawn procesow.
        web.loadHTMLString("<html><body></body></html>", baseURL: nil)
        warmupWebView = web
        NSLog("[TentaFlow] WebKit warmup zlecony")
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
        lastBackgroundTime = Date()
        NSLog("[TentaFlow] didEnterBackground — Rust idzie w pause")
        tentaflow_on_pause()
    }

    func applicationWillEnterForeground(_ application: UIApplication) {
        let elapsed: TimeInterval
        if let t = lastBackgroundTime {
            elapsed = Date().timeIntervalSince(t)
        } else {
            elapsed = -1
        }
        NSLog("[TentaFlow] willEnterForeground — po \(String(format: "%.1f", elapsed))s w tle")

        tentaflow_on_resume()

        // Powiadom ContentView.Coordinator zeby sprawdzil serwer i
        // zrobil reload WKWebView albo wypchnal splash + pollServer.
        NotificationCenter.default.post(
            name: .tentaflowForegrounded,
            object: nil,
            userInfo: ["elapsed": elapsed]
        )
    }

    func applicationDidReceiveMemoryWarning(_ application: UIApplication) {
        tentaflow_on_memory_warning()
    }
}

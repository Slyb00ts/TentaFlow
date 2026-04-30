// =============================================================================
// Plik: ContentView.swift
// Opis: Glowny widok iOS — WKWebView ladujacy dashboard z lokalnego HTTPS serwera.
//       Rust core (serwer + mesh + inference) dziala w tle.
// =============================================================================

import SwiftUI
import WebKit

struct ContentView: View {
    @State private var isLoading = true
    @State private var statusText = "Uruchamianie serwera..."
    @State private var retryCount = 0

    var body: some View {
        ZStack {
            // Czarne tlo pod status barem i na dole (home indicator)
            Color.black.ignoresSafeArea()

            TentaFlowWebView(isLoading: $isLoading, statusText: $statusText, retryCount: $retryCount)
                .ignoresSafeArea(edges: .bottom)

            // Splash screen podczas ladowania serwera
            if isLoading {
                ZStack {
                    Color.black.ignoresSafeArea()
                    VStack(spacing: 16) {
                        ProgressView()
                            .progressViewStyle(CircularProgressViewStyle(tint: .white))
                            .scaleEffect(1.5)
                        Text("TentaFlow")
                            .foregroundColor(.white)
                            .font(.title2)
                        Text(statusText)
                            .foregroundColor(.gray)
                            .font(.caption)
                        if retryCount > 0 {
                            Text("Proba \(retryCount)...")
                                .foregroundColor(.gray.opacity(0.6))
                                .font(.caption2)
                        }
                    }
                }
            }
        }
    }
}

#if os(iOS)
struct TentaFlowWebView: UIViewRepresentable {
    @Binding var isLoading: Bool
    @Binding var statusText: String
    @Binding var retryCount: Int

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()

        // Wspoldzielony process pool — reuzywa procesy WebKit prewarm w
        // AppDelegate.didFinishLaunching. Bez tego kazdy WKWebView spawnuje
        // swoje GPU/WebContent/Networking procesy (4.5s kazdy).
        config.processPool = AppDelegate.sharedProcessPool

        // Wylacz cache WKWebView — pliki statyczne sa wkompilowane w binarke
        // i musza byc ladowane z serwera zawsze na swiezo po aktualizacji
        config.websiteDataStore = WKWebsiteDataStore.nonPersistent()

        // Kamera / mikrofon w JS (getUserMedia dla QR scanner i vision preview).
        // Bez tych dwoch flag WKWebView nie pozwoli odtworzyc strumienia video
        // i wymaga gestu uzytkownika przed kazdym play().
        config.allowsInlineMediaPlayback = true
        config.mediaTypesRequiringUserActionForPlayback = []

        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        webView.isOpaque = false
        webView.backgroundColor = .black
        webView.scrollView.contentInsetAdjustmentBehavior = .never

        // Zapamietaj instancje w Coordinator — potrzebna przy wake zeby
        // zrobic webView.reload() z handlera tentaflowForegrounded.
        context.coordinator.managedWebView = webView

        // Sprawdzaj gotowość serwera zanim załadujesz strone
        context.coordinator.pollServer(webView: webView)
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, URLSessionDelegate {
        let parent: TentaFlowWebView
        private var attempts = 0

        /// Referencja na realny WKWebView — potrzebna przy wake zeby wykonac
        /// reload(). Weak zeby nie zatrzymywac view w pamieci po deinit.
        weak var managedWebView: WKWebView?

        init(parent: TentaFlowWebView) {
            self.parent = parent
            super.init()

            // Subskrybuj event z AppDelegate — emitowany w applicationWillEnterForeground.
            NotificationCenter.default.addObserver(
                self,
                selector: #selector(handleForegrounded(_:)),
                name: .tentaflowForegrounded,
                object: nil
            )
        }

        deinit {
            NotificationCenter.default.removeObserver(self)
        }

        @objc private func handleForegrounded(_ note: Notification) {
            guard let webView = managedWebView else { return }
            let elapsed = (note.userInfo?["elapsed"] as? TimeInterval) ?? -1
            NSLog("[TentaFlow] foreground resume — sprawdzam serwer (suspend \(String(format: "%.1f", elapsed))s)")
            refreshAfterResume(webView: webView)
        }

        /// Po wybudzeniu apki: szybki HTTPS probe 1.5s. Jesli serwer zywy ->
        /// webView.reload() (odswieza stale SSE/WebSocket po suspendzie).
        /// Jesli nie -> splash + pelny pollServer.
        func refreshAfterResume(webView: WKWebView) {
            let sessionConfig = URLSessionConfiguration.ephemeral
            sessionConfig.timeoutIntervalForRequest = 1.5
            let session = URLSession(configuration: sessionConfig, delegate: self, delegateQueue: nil)

            guard let url = URL(string: "https://127.0.0.1:8090") else { return }
            let task = session.dataTask(with: url) { [weak self] _, response, error in
                guard let self = self else { return }

                if let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode > 0 {
                    DispatchQueue.main.async {
                        NSLog("[TentaFlow] resume: serwer zywy, webView.reload()")
                        webView.reload()
                    }
                } else {
                    let desc = error?.localizedDescription ?? "brak odpowiedzi"
                    NSLog("[TentaFlow] resume: serwer martwy (\(desc)) — pollServer")
                    DispatchQueue.main.async {
                        self.attempts = 0
                        self.parent.isLoading = true
                        self.parent.statusText = "Odnawianie polaczenia..."
                        self.parent.retryCount = 0
                        self.pollServer(webView: webView)
                    }
                }
            }
            task.resume()
        }

        // iOS 15+ — WKWebView kieruje wszystkie prosby o kamere/mikrofon do
        // WKUIDelegate. Bez tej metody getUserMedia() dostaje NotAllowedError,
        // nawet z NSCameraUsageDescription w Info.plist. Grantujemy bezwarunkowo,
        // bo strona jest serwowana z localhost w naszej wlasnej binarce.
        func webView(_ webView: WKWebView,
                     requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                     initiatedByFrame frame: WKFrameInfo,
                     type: WKMediaCaptureType,
                     decisionHandler: @escaping (WKPermissionDecision) -> Void) {
            decisionHandler(.grant)
        }

        /// Sprawdza czy serwer HTTPS odpowiada zanim załadujemy WKWebView
        func pollServer(webView: WKWebView) {
            attempts += 1
            let attempt = attempts

            DispatchQueue.main.async {
                self.parent.retryCount = attempt
                if attempt <= 3 {
                    self.parent.statusText = "Uruchamianie serwera..."
                } else if attempt <= 10 {
                    self.parent.statusText = "Oczekiwanie na serwer..."
                } else {
                    self.parent.statusText = "Serwer nie odpowiada (proba \(attempt))"
                }
            }

            let sessionConfig = URLSessionConfiguration.ephemeral
            sessionConfig.timeoutIntervalForRequest = 3
            let session = URLSession(configuration: sessionConfig, delegate: self, delegateQueue: nil)

            guard let url = URL(string: "https://127.0.0.1:8090") else { return }
            let task = session.dataTask(with: url) { [weak self] _, response, error in
                guard let self = self else { return }

                if let httpResponse = response as? HTTPURLResponse, httpResponse.statusCode > 0 {
                    // Serwer odpowiada — laduj strone w WKWebView
                    DispatchQueue.main.async {
                        self.parent.statusText = "Ladowanie dashboardu..."
                        var request = URLRequest(url: url)
                        request.cachePolicy = .reloadIgnoringLocalCacheData
                        webView.load(request)
                    }
                } else {
                    // Serwer jeszcze nie gotowy — ponow po 1.5s
                    let errorDesc = error?.localizedDescription ?? "brak odpowiedzi"
                    print("[TentaFlow] Serwer niedostepny (proba \(attempt)): \(errorDesc)")

                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                        self.pollServer(webView: webView)
                    }
                }
            }
            task.resume()
        }

        // Akceptuj self-signed cert w URLSession (health check)
        func urlSession(_ session: URLSession,
                        didReceive challenge: URLAuthenticationChallenge,
                        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
            if challenge.protectionSpace.host == "127.0.0.1",
               let trust = challenge.protectionSpace.serverTrust {
                completionHandler(.useCredential, URLCredential(trust: trust))
                return
            }
            completionHandler(.performDefaultHandling, nil)
        }

        // Akceptuj self-signed cert na localhost (WKWebView)
        func webView(_ webView: WKWebView,
                     didReceive challenge: URLAuthenticationChallenge,
                     completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
            if challenge.protectionSpace.host == "127.0.0.1" {
                if let trust = challenge.protectionSpace.serverTrust {
                    completionHandler(.useCredential, URLCredential(trust: trust))
                    return
                }
            }
            completionHandler(.performDefaultHandling, nil)
        }

        // Ukryj splash po zaladowaniu strony + wstrzyknij viewport meta tag
        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            // Wymusz viewport meta tag jesli dashboard go nie ma
            let viewportJS = """
            (function() {
                var meta = document.querySelector('meta[name="viewport"]');
                if (!meta) {
                    meta = document.createElement('meta');
                    meta.name = 'viewport';
                    document.head.appendChild(meta);
                }
                meta.content = 'width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no, viewport-fit=cover';
            })();
            """
            webView.evaluateJavaScript(viewportJS)

            DispatchQueue.main.async {
                self.parent.isLoading = false
            }
        }

        // Przy bledzie — sprobuj ponownie
        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            print("[TentaFlow] WKWebView didFail: \(error.localizedDescription)")
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
                self.pollServer(webView: webView)
            }
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            print("[TentaFlow] WKWebView didFailProvisional: \(error.localizedDescription)")
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
                self.pollServer(webView: webView)
            }
        }
    }
}
#endif

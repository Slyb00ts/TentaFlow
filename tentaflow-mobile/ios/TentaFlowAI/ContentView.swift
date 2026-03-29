// =============================================================================
// Plik: ContentView.swift
// Opis: Glowny widok iOS — WKWebView ladujacy dashboard z lokalnego HTTPS serwera.
//       Rust core (serwer + mesh + inference) dziala w tle.
// =============================================================================

import SwiftUI
import WebKit

struct ContentView: View {
    @State private var isLoading = true

    var body: some View {
        ZStack {
            TentaFlowWebView(isLoading: $isLoading)
                .ignoresSafeArea()

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
                        Text("Uruchamianie serwera...")
                            .foregroundColor(.gray)
                            .font(.caption)
                    }
                }
            }
        }
    }
}

#if os(iOS)
struct TentaFlowWebView: UIViewRepresentable {
    @Binding var isLoading: Bool

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()

        // Wylacz cache WKWebView — pliki statyczne sa wkompilowane w binarke
        // i musza byc ladowane z serwera zawsze na swiezo po aktualizacji
        config.websiteDataStore = WKWebsiteDataStore.nonPersistent()

        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = context.coordinator
        webView.isOpaque = false
        webView.backgroundColor = .black

        // Poczekaj na start serwera Rust, potem laduj
        DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
            if let url = URL(string: "https://127.0.0.1:8090") {
                var request = URLRequest(url: url)
                request.cachePolicy = .reloadIgnoringLocalCacheData
                webView.load(request)
            }
        }
        return webView
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    class Coordinator: NSObject, WKNavigationDelegate {
        let parent: TentaFlowWebView

        init(parent: TentaFlowWebView) {
            self.parent = parent
        }

        // Akceptuj self-signed cert na localhost
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

        // Ukryj splash po zaladowaniu strony
        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            DispatchQueue.main.async {
                self.parent.isLoading = false
            }
        }

        // Przy bledzie — sprobuj ponownie po 2s
        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
                if let url = URL(string: "https://127.0.0.1:8090") {
                    webView.load(URLRequest(url: url))
                }
            }
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            // Serwer jeszcze nie gotowy — ponow probe
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
                if let url = URL(string: "https://127.0.0.1:8090") {
                    webView.load(URLRequest(url: url))
                }
            }
        }
    }
}
#endif

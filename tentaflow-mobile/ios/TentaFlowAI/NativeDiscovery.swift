// =============================================================================
// Plik: NativeDiscovery.swift
// Opis: Natywny mDNS browser/advertiser na iOS — uzywa Apple Network framework
//       zamiast mdns-sd crate (ktory nie dziala na iOS z raw UDP multicast).
//       Rejestruje serwis i odkrywa inne hosty w sieci lokalnej.
// =============================================================================

import Foundation
import Network

class NativeDiscovery {
    static let shared = NativeDiscovery()

    private var browser: NWBrowser?
    private var netService: NetService?
    private var nodeId: String = ""
    private var port: UInt16 = 8090

    private init() {}

    /// Uruchom mDNS discovery i advertising
    func start(nodeId: String, port: UInt16) {
        self.nodeId = nodeId
        self.port = port

        startAdvertising()
        startBrowsing()
    }

    /// Reklamuj serwis w sieci lokalnej z portem 8090 (QUIC/HTTPS)
    private func startAdvertising() {
        let serviceName = "tentaflow-\(String(nodeId.prefix(8)))"

        // NetService pozwala reklamowac dowolny port (w przeciwienstwie do NWListener)
        netService = NetService(
            domain: "local.",
            type: "_tentaflow-mesh._udp.",
            name: serviceName,
            port: Int32(port)
        )

        // TXT record z metadanymi
        let txtData: [String: Data] = [
            "version": "1".data(using: .utf8)!,
            "role": "mobile".data(using: .utf8)!,
            "node_id": nodeId.data(using: .utf8)!,
        ]
        netService?.setTXTRecord(NetService.data(fromTXTRecord: txtData))

        netService?.publish()
        print("[NativeDiscovery] Advertising: \(serviceName) na porcie \(port)")
    }

    /// Szukaj innych hostow w sieci
    private func startBrowsing() {
        let descriptor = NWBrowser.Descriptor.bonjour(type: "_tentaflow-mesh._udp.", domain: nil)
        let params = NWParameters()
        params.includePeerToPeer = true

        browser = NWBrowser(for: descriptor, using: params)

        browser?.browseResultsChangedHandler = { results, changes in
            for result in results {
                switch result.endpoint {
                case .service(let name, let type, let domain, let interface):
                    print("[NativeDiscovery] Znaleziono: \(name) (\(type)) w \(domain ?? "local") na \(interface?.name ?? "?")")

                    // Resolve endpoint zeby poznac IP
                    let connection = NWConnection(to: result.endpoint, using: .udp)
                    connection.stateUpdateHandler = { state in
                        if case .ready = state {
                            if let path = connection.currentPath,
                               let endpoint = path.remoteEndpoint {
                                print("[NativeDiscovery] Resolved: \(name) -> \(endpoint)")
                            }
                            connection.cancel()
                        }
                    }
                    connection.start(queue: .global(qos: .utility))

                default:
                    break
                }
            }
        }

        browser?.stateUpdateHandler = { state in
            switch state {
            case .ready:
                print("[NativeDiscovery] Browsing aktywny — szukam hostow...")
            case .failed(let error):
                print("[NativeDiscovery] Browsing BLAD: \(error)")
            default:
                break
            }
        }

        browser?.start(queue: .global(qos: .utility))
    }

    func stop() {
        browser?.cancel()
        netService?.stop()
        browser = nil
        netService = nil
    }
}

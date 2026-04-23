// =============================================================================
// Plik: NativeDiscovery.swift
// Opis: Natywny LAN discovery przez Bonjour (Network.framework). iOS blokuje
//       raw multicast UDP bez Apple entitlementa, wiec iroh mDNS w Rust tu
//       nie dziala. NWBrowser/NetService ida przez systemowy mDNSResponder,
//       nie wymagaja entitlementa. Znalezionych peerow karmimy do iroh przez
//       FFI tentaflow_mobile_add_discovered_peer, ktore woluje iroh
//       connect_to_peer_direct z explicit IP+port.
//
//       Service type pasuje do iroh (`_irohv1._udp`) — dzieki temu iOS widzi
//       urzadzenia Linux/Windows/macOS uzywajace tego samego iroh mDNS,
//       mimo ze sam iOS nie moze mDNS nadawac przez raw socket.
// =============================================================================

import Foundation
import Network

/// Iroh Bonjour service type. Bez koncowego `_udp.local.` — NWBrowser.Descriptor
/// dodaje `._udp.local.` sam. Musi sie zgadzac z `N0_SERVICE_NAME = "irohv1"` w
/// `iroh-0.98/src/address_lookup/mdns.rs`.
private let IROH_SERVICE_TYPE = "_irohv1._udp"

class NativeDiscovery {
    static let shared = NativeDiscovery()

    private var irohBrowser: NWBrowser?
    private var nodeId: String = ""
    private var port: UInt16 = 8090

    // Cache rezolwowanych peerow — zeby nie spamowac FFI przy kazdym
    // browseResultsChangedHandler (moze byc wolany wielokrotnie).
    private var resolvedEndpoints = Set<String>()
    private let resolveQueue = DispatchQueue(label: "ai.tentaflow.discovery.resolve")

    private init() {}

    /// Uruchom LAN discovery — browsuje iroh Bonjour service.
    /// Advertising robi Rust iroh na desktopie. Na iOS nie robimy advertisingu
    /// z Swift, bo iroh w Rust na iOS nie moze nadawac mDNS (raw multicast
    /// blocked by kernel). iOS bedzie dostepne dla innych iOS-ow przez relay
    /// iroh po tym jak choc raz peer nas zna z druga strone.
    func start(nodeId: String, port: UInt16) {
        self.nodeId = nodeId
        self.port = port

        startBrowsing()
    }

    /// Szukaj iroh peerow przez systemowy Bonjour.
    private func startBrowsing() {
        let descriptor = NWBrowser.Descriptor.bonjour(type: IROH_SERVICE_TYPE, domain: nil)
        let params = NWParameters()
        params.includePeerToPeer = true

        let browser = NWBrowser(for: descriptor, using: params)
        self.irohBrowser = browser

        browser.browseResultsChangedHandler = { [weak self] results, _ in
            guard let self = self else { return }
            for result in results {
                guard case .service(let name, _, _, _) = result.endpoint else { continue }

                // Nazwa instancji = base32-nopad lowercase EndpointId (52 znaki).
                // Unikaj wielokrotnego rozwiazywania tego samego peera.
                self.resolveQueue.async {
                    if self.resolvedEndpoints.contains(name) {
                        return
                    }
                    self.resolvedEndpoints.insert(name)
                    DispatchQueue.global(qos: .utility).async {
                        self.resolvePeer(endpointIdBase32: name, serviceEndpoint: result.endpoint)
                    }
                }
            }
        }

        browser.stateUpdateHandler = { state in
            switch state {
            case .ready:
                print("[NativeDiscovery] Browsuje \(IROH_SERVICE_TYPE) — LAN discovery aktywne")
            case .failed(let error):
                print("[NativeDiscovery] Browser BLAD: \(error)")
            case .cancelled:
                print("[NativeDiscovery] Browser zamkniety")
            default:
                break
            }
        }

        browser.start(queue: .global(qos: .utility))
    }

    /// Rozwiazuje endpoint Bonjour do SocketAddr (IPv4 + port) i przekazuje
    /// do Rust przez FFI. Wlasna nazwa iroh instance jest pomijana.
    private func resolvePeer(endpointIdBase32: String, serviceEndpoint: NWEndpoint) {
        let connection = NWConnection(to: serviceEndpoint, using: .udp)
        connection.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                if let path = connection.currentPath,
                   let remote = path.remoteEndpoint,
                   case .hostPort(let host, let port) = remote {
                    let ipString = NativeDiscovery.hostToIPString(host)
                    if let ip = ipString {
                        self.deliverToRust(endpointIdBase32: endpointIdBase32,
                                           ipString: ip,
                                           port: port.rawValue)
                    } else {
                        print("[NativeDiscovery] Nie udalo sie wyciagnac IP dla \(endpointIdBase32)")
                    }
                }
                connection.cancel()
            case .failed(let error):
                print("[NativeDiscovery] Resolve failed dla \(endpointIdBase32): \(error)")
                // Odblokuj retry jesli sie nie udalo — moze peer wrocil na inny adres.
                self.resolveQueue.async {
                    self.resolvedEndpoints.remove(endpointIdBase32)
                }
                connection.cancel()
            default:
                break
            }
        }
        connection.start(queue: .global(qos: .utility))
    }

    /// Zamienia NWEndpoint.Host na string IP. IPv4 prefereowane nad IPv6,
    /// bo iroh na LAN-ie uzywa UDP v4 domyslnie.
    private static func hostToIPString(_ host: NWEndpoint.Host) -> String? {
        switch host {
        case .ipv4(let addr):
            return "\(addr)"
        case .ipv6(let addr):
            // Pomin link-local fe80::
            let s = "\(addr)"
            if s.hasPrefix("fe80:") {
                return nil
            }
            return s
        case .name(let hostname, _):
            return hostname
        @unknown default:
            return nil
        }
    }

    /// Woluje Rust FFI przez C-stringi.
    private func deliverToRust(endpointIdBase32: String, ipString: String, port: UInt16) {
        // Pomin wlasny node — advertising local service wraca do nas samych.
        // Rust FFI i tak by odrzucil (is_connected check albo connect do siebie),
        // ale lepiej nie palic cykli.
        if endpointIdBase32.lowercased() == nodeIdToBase32(nodeId) {
            return
        }

        print("[NativeDiscovery] Peer: \(endpointIdBase32.prefix(12))… @ \(ipString):\(port)")

        var delivered = false
        endpointIdBase32.withCString { epPtr in
            ipString.withCString { ipPtr in
                delivered = tentaflow_mobile_add_discovered_peer(epPtr, ipPtr, port)
            }
        }

        if !delivered {
            // Mesh jeszcze nie gotowy albo connect odrzucony. Zdejmij z cache
            // zeby nastepny browseResultsChangedHandler sprobowal ponownie.
            resolveQueue.async { [endpointIdBase32] in
                self.resolvedEndpoints.remove(endpointIdBase32)
            }
            print("[NativeDiscovery] FFI zwrocil false — retry przy nastepnym update")
        }
    }

    /// Konwersja hex node_id → base32 lowercase (iroh instance format).
    /// Uzywane tylko do porownania self vs peer.
    private func nodeIdToBase32(_ hex: String) -> String {
        guard hex.count == 64 else { return "" }
        var bytes = [UInt8]()
        bytes.reserveCapacity(32)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let b = UInt8(hex[index..<next], radix: 16) else { return "" }
            bytes.append(b)
            index = next
        }
        return NativeDiscovery.base32NopadLowercase(bytes)
    }

    /// RFC 4648 Base32 bez paddingu, lowercase (dokladnie jak iroh:
    /// `data_encoding::BASE32_NOPAD.encode(...).to_ascii_lowercase()`).
    private static func base32NopadLowercase(_ bytes: [UInt8]) -> String {
        let alphabet = Array("abcdefghijklmnopqrstuvwxyz234567")
        var result = ""
        var buffer: UInt32 = 0
        var bits: Int = 0
        for byte in bytes {
            buffer = (buffer << 8) | UInt32(byte)
            bits += 8
            while bits >= 5 {
                bits -= 5
                let idx = Int((buffer >> UInt32(bits)) & 0x1f)
                result.append(alphabet[idx])
            }
        }
        if bits > 0 {
            let idx = Int((buffer << UInt32(5 - bits)) & 0x1f)
            result.append(alphabet[idx])
        }
        return result
    }

    func stop() {
        irohBrowser?.cancel()
        irohBrowser = nil
        resolveQueue.async {
            self.resolvedEndpoints.removeAll()
        }
    }
}

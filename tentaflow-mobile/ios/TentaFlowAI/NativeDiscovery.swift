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
    private var started = false

    // Cache rezolwowanych peerow — zeby nie spamowac FFI przy kazdym
    // browseResultsChangedHandler (moze byc wolany wielokrotnie).
    // Mapa: endpointIdBase32 -> liczba proub FFI (dla backoff).
    private var resolvedEndpoints = [String: Int]()
    // Kolejka peerow ktore czekaja na gotowosc Rust mesh (FFI zwrocil false).
    private var pendingPeers = [String: (endpoint: NWEndpoint, attempts: Int)]()
    private let resolveQueue = DispatchQueue(label: "ai.tentaflow.discovery.resolve")

    // Watchdog — co 10s loguje stan browsera i ile peerow zostalo znalezionych.
    private var watchdogTimer: DispatchSourceTimer?
    private var lastResultsCount = 0
    private var peersDeliveredToRust = 0

    private init() {}

    /// Uruchom LAN discovery — browsuje iroh Bonjour service.
    ///
    /// Advertising robi Rust iroh na desktopie. Na iOS nie robimy advertisingu
    /// z Swift, bo iroh w Rust na iOS nie moze nadawac mDNS (raw multicast
    /// blocked by kernel).
    ///
    /// UWAGA: wywoluj DOPIERO po tentaflow_mobile_start() i dodaniu 1-2s
    /// opoznienia, zeby Rust MESH_HANDLE zdazyl sie zainicjalizowac. Inaczej
    /// pierwsze FFI add_discovered_peer zwroci false (mesh niegotowy).
    func start(nodeId: String, port: UInt16) {
        if started {
            NSLog("[NativeDiscovery] start() ponowne — ignoruje")
            return
        }
        started = true
        self.nodeId = nodeId
        self.port = port

        NSLog("[NativeDiscovery] Startuje na typie \(IROH_SERVICE_TYPE)")
        startBrowsing()
        startWatchdog()
        startRetryLoop()
    }

    /// Szukaj iroh peerow przez systemowy Bonjour. Browser startuje na main
    /// queue — to wymusza wyswietlenie promptu "Local Network" od iOS.
    private func startBrowsing() {
        let descriptor = NWBrowser.Descriptor.bonjour(type: IROH_SERVICE_TYPE, domain: nil)
        let params = NWParameters()
        params.includePeerToPeer = true

        let browser = NWBrowser(for: descriptor, using: params)
        self.irohBrowser = browser

        browser.browseResultsChangedHandler = { [weak self] results, changes in
            guard let self = self else { return }
            NSLog("[NativeDiscovery] browseResultsChangedHandler: %d wynik(ow), %d zmian(a)",
                  results.count, changes.count)
            self.lastResultsCount = results.count

            for change in changes {
                switch change {
                case .added(let result):
                    NSLog("[NativeDiscovery] ADD: \(self.describeEndpoint(result.endpoint))")
                case .removed(let result):
                    NSLog("[NativeDiscovery] REM: \(self.describeEndpoint(result.endpoint))")
                case .changed(_, let result, _):
                    NSLog("[NativeDiscovery] CHG: \(self.describeEndpoint(result.endpoint))")
                case .identical:
                    break
                @unknown default:
                    break
                }
            }

            for result in results {
                guard case .service(let name, _, _, _) = result.endpoint else { continue }

                self.resolveQueue.async {
                    // Cache po nazwie instancji = EndpointId w base32.
                    if self.resolvedEndpoints[name] != nil {
                        return
                    }
                    self.resolvedEndpoints[name] = 0
                    DispatchQueue.global(qos: .utility).async {
                        self.resolvePeer(endpointIdBase32: name, serviceEndpoint: result.endpoint)
                    }
                }
            }
        }

        browser.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .setup:
                NSLog("[NativeDiscovery] state=setup")
            case .ready:
                NSLog("[NativeDiscovery] state=READY — browsing \(IROH_SERVICE_TYPE) aktywny")
            case .failed(let error):
                NSLog("[NativeDiscovery] state=FAILED: \(error.localizedDescription)")
                // Po porazce sprobuj restartowac za 3s (permission/network issue).
                DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) {
                    NSLog("[NativeDiscovery] Restart po failure...")
                    self.irohBrowser?.cancel()
                    self.startBrowsing()
                }
            case .cancelled:
                NSLog("[NativeDiscovery] state=cancelled")
            case .waiting(let error):
                NSLog("[NativeDiscovery] state=WAITING: \(error.localizedDescription)")
            @unknown default:
                NSLog("[NativeDiscovery] state=unknown")
            }
        }

        // Main queue - wymusza wyswietlenie promptu "Local Network" od iOS
        // przy pierwszej probie multicastu. Inne kolejki moga nie wyzwolic UI.
        browser.start(queue: .main)
        NSLog("[NativeDiscovery] browser.start(queue: .main) wywolany")
    }

    /// Watchdog co 10s — loguje stan systemu (aby user widzial w konsoli Xcode
    /// czy cokolwiek sie dzieje). Pomaga zdiagnozowac: prompt nie pokazany,
    /// permission denied, czy po prostu brak peerow w sieci.
    private func startWatchdog() {
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        timer.schedule(deadline: .now() + 10.0, repeating: 10.0)
        timer.setEventHandler { [weak self] in
            guard let self = self else { return }
            let state = self.irohBrowser?.state.debugDescription ?? "nil"
            self.resolveQueue.sync {
                NSLog(
                    "[NativeDiscovery] WATCHDOG: browser=\(state) results=\(self.lastResultsCount) resolved=\(self.resolvedEndpoints.count) delivered=\(self.peersDeliveredToRust) pending=\(self.pendingPeers.count)"
                )
            }
        }
        timer.resume()
        watchdogTimer = timer
    }

    /// Retry loop — co 2s sprobuj dostarczyc pending peerow do Rust.
    /// Peer trafia do pending gdy FFI zwraca false (mesh nie gotowy).
    private func startRetryLoop() {
        resolveQueue.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            self?.retryPendingPeers()
        }
    }

    private func retryPendingPeers() {
        let snapshot: [(String, NWEndpoint, Int)] = pendingPeers.map { ($0.key, $0.value.endpoint, $0.value.attempts) }
        if !snapshot.isEmpty {
            NSLog("[NativeDiscovery] Retry \(snapshot.count) pending peer(s)")
        }
        for (name, endpoint, attempts) in snapshot {
            if attempts > 20 {
                NSLog("[NativeDiscovery] Poddaje sie dla \(name.prefix(12))… po 20 probach")
                pendingPeers.removeValue(forKey: name)
                resolvedEndpoints.removeValue(forKey: name)
                continue
            }
            pendingPeers[name] = (endpoint: endpoint, attempts: attempts + 1)
            DispatchQueue.global(qos: .utility).async { [weak self] in
                self?.resolvePeer(endpointIdBase32: name, serviceEndpoint: endpoint)
            }
        }
        // Reschedule.
        resolveQueue.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            self?.retryPendingPeers()
        }
    }

    /// Rozwiazuje endpoint Bonjour do SocketAddr (IPv4 + port) i przekazuje
    /// do Rust przez FFI. Wlasna nazwa iroh instance jest pomijana.
    private func resolvePeer(endpointIdBase32: String, serviceEndpoint: NWEndpoint) {
        NSLog("[NativeDiscovery] Rozwiazuje \(endpointIdBase32.prefix(12))…")
        let connection = NWConnection(to: serviceEndpoint, using: .udp)
        connection.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                if let path = connection.currentPath,
                   let remote = path.remoteEndpoint {
                    NSLog("[NativeDiscovery] Resolve OK dla \(endpointIdBase32.prefix(12))… remote=\(remote.debugDescription)")
                    self.handleResolvedEndpoint(endpointIdBase32: endpointIdBase32,
                                                 remote: remote,
                                                 serviceEndpoint: serviceEndpoint)
                } else {
                    NSLog("[NativeDiscovery] Resolve READY bez remoteEndpoint dla \(endpointIdBase32.prefix(12))…")
                }
                connection.cancel()
            case .failed(let error):
                NSLog("[NativeDiscovery] Resolve FAIL \(endpointIdBase32.prefix(12))…: \(error)")
                // Daj retry na kolejnym tickzie watchdoga — pozwol NWBrowser wykryc ponownie.
                self.resolveQueue.async {
                    self.resolvedEndpoints.removeValue(forKey: endpointIdBase32)
                }
                connection.cancel()
            case .waiting(let error):
                NSLog("[NativeDiscovery] Resolve WAITING \(endpointIdBase32.prefix(12))…: \(error)")
            case .preparing:
                break
            default:
                break
            }
        }
        connection.start(queue: .global(qos: .utility))
    }

    /// Z rozwiazanego endpointu wyciagnij IP+port, zrob FFI do Rust. Jesli
    /// remote to hostname (typu "host.local"), rozwiaz go przez getaddrinfo.
    private func handleResolvedEndpoint(endpointIdBase32: String,
                                        remote: NWEndpoint,
                                        serviceEndpoint: NWEndpoint) {
        guard case .hostPort(let host, let port) = remote else {
            NSLog("[NativeDiscovery] Remote endpoint nie hostPort: \(remote.debugDescription)")
            return
        }

        switch host {
        case .ipv4(let addr):
            deliverToRust(endpointIdBase32: endpointIdBase32,
                          ipString: "\(addr)",
                          port: port.rawValue,
                          serviceEndpoint: serviceEndpoint)
        case .ipv6(let addr):
            let s = "\(addr)"
            if s.hasPrefix("fe80:") {
                NSLog("[NativeDiscovery] IPv6 link-local pominiety dla \(endpointIdBase32.prefix(12))…")
                return
            }
            deliverToRust(endpointIdBase32: endpointIdBase32,
                          ipString: s,
                          port: port.rawValue,
                          serviceEndpoint: serviceEndpoint)
        case .name(let hostname, _):
            NSLog("[NativeDiscovery] Remote to hostname (\(hostname)) — rozwiazuje przez getaddrinfo")
            if let ip = NativeDiscovery.resolveHostname(hostname) {
                NSLog("[NativeDiscovery] \(hostname) -> \(ip)")
                deliverToRust(endpointIdBase32: endpointIdBase32,
                              ipString: ip,
                              port: port.rawValue,
                              serviceEndpoint: serviceEndpoint)
            } else {
                NSLog("[NativeDiscovery] getaddrinfo FAIL dla \(hostname)")
            }
        @unknown default:
            NSLog("[NativeDiscovery] Nieznany typ host dla \(endpointIdBase32.prefix(12))…")
        }
    }

    /// Blokujacy getaddrinfo — rozwiaze hostname .local przez systemowe DNS
    /// (mDNS responder). Zwraca pierwszy IPv4 adres.
    private static func resolveHostname(_ hostname: String) -> String? {
        var hints = addrinfo(
            ai_flags: 0,
            ai_family: AF_INET,
            ai_socktype: SOCK_DGRAM,
            ai_protocol: IPPROTO_UDP,
            ai_addrlen: 0,
            ai_canonname: nil,
            ai_addr: nil,
            ai_next: nil
        )
        var result: UnsafeMutablePointer<addrinfo>?
        let err = getaddrinfo(hostname, nil, &hints, &result)
        if err != 0 || result == nil {
            return nil
        }
        defer { freeaddrinfo(result) }

        var node = result
        while let current = node {
            if let sa = current.pointee.ai_addr {
                var addrBuf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                if sa.pointee.sa_family == sa_family_t(AF_INET) {
                    sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { sin in
                        var inaddr = sin.pointee.sin_addr
                        inet_ntop(AF_INET, &inaddr, &addrBuf, socklen_t(INET_ADDRSTRLEN))
                    }
                    return String(cString: addrBuf)
                }
            }
            node = current.pointee.ai_next
        }
        return nil
    }

    /// Woluje Rust FFI przez C-stringi. Jesli mesh niegotowy (false) —
    /// dodaje do pendingPeers zeby retry loop sprobowal ponownie co 2s.
    private func deliverToRust(endpointIdBase32: String,
                               ipString: String,
                               port: UInt16,
                               serviceEndpoint: NWEndpoint) {
        // Pomin wlasny node — advertising local service wraca do nas samych.
        if endpointIdBase32.lowercased() == nodeIdToBase32(nodeId) {
            NSLog("[NativeDiscovery] Pomijam siebie: \(endpointIdBase32.prefix(12))…")
            resolveQueue.async {
                self.pendingPeers.removeValue(forKey: endpointIdBase32)
            }
            return
        }

        NSLog("[NativeDiscovery] FFI: \(endpointIdBase32.prefix(12))… @ \(ipString):\(port)")

        var delivered = false
        endpointIdBase32.withCString { epPtr in
            ipString.withCString { ipPtr in
                delivered = tentaflow_mobile_add_discovered_peer(epPtr, ipPtr, port)
            }
        }

        resolveQueue.async {
            if delivered {
                NSLog("[NativeDiscovery] FFI OK — peer dodany")
                self.peersDeliveredToRust += 1
                self.pendingPeers.removeValue(forKey: endpointIdBase32)
            } else {
                NSLog("[NativeDiscovery] FFI false — mesh niegotowy, dodaje do pending")
                // Zostaw w pending — retry loop sprobuje ponownie za 2s.
                let prev = self.pendingPeers[endpointIdBase32]?.attempts ?? 0
                self.pendingPeers[endpointIdBase32] = (endpoint: serviceEndpoint, attempts: prev)
            }
        }
    }

    /// Konwersja hex node_id → base32 lowercase (iroh instance format).
    /// Uzywane tylko do porownania self vs peer. Jesli nodeId nie jest hex
    /// (np. UUID), zwroci pusty string — porownanie da false, OK (i tak nie
    /// chcemy siebie dodawac do mesh, a Rust FFI odrzuci self-connect).
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

    /// Diagnostyczny opis endpointu dla logow.
    private func describeEndpoint(_ ep: NWEndpoint) -> String {
        switch ep {
        case .service(let name, let type, let domain, let iface):
            let ifaceStr = iface?.name ?? "?"
            return "service(name=\(name) type=\(type) domain=\(domain) iface=\(ifaceStr))"
        case .hostPort(let host, let port):
            return "hostPort(\(host):\(port))"
        case .url(let url):
            return "url(\(url))"
        case .unix(let path):
            return "unix(\(path))"
        case .opaque:
            return "opaque"
        @unknown default:
            return "unknown"
        }
    }

    func stop() {
        watchdogTimer?.cancel()
        watchdogTimer = nil
        irohBrowser?.cancel()
        irohBrowser = nil
        resolveQueue.async {
            self.resolvedEndpoints.removeAll()
            self.pendingPeers.removeAll()
        }
        started = false
    }
}

// Pomocniczy describe dla NWBrowser.State.
extension NWBrowser.State {
    var debugDescription: String {
        switch self {
        case .setup: return "setup"
        case .ready: return "ready"
        case .failed(let e): return "failed(\(e))"
        case .cancelled: return "cancelled"
        case .waiting(let e): return "waiting(\(e))"
        @unknown default: return "unknown"
        }
    }
}

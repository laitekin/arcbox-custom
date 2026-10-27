import AppKit
import ArcBoxClient
import Foundation
import Network

/// App-owned HTTP + WebSocket gateway. Only loopback, authenticated per machine/session.
@MainActor
final class WebDesktopServer {
    static let shared = WebDesktopServer()
    private var listener: NWListener?
    private var port: UInt16 = 0
    private var sessions: [String: WebDesktopSession] = [:]
    private var connections: [UUID: WebDesktopHTTPConnection] = [:]
    private var activeToken: String?
    private var maintenance: Task<Void, Never>?
    private var starting = false

    var origin: String { "http://127.0.0.1:\(port)" }

    func open(machine: MachineViewModel, forward: MachinePortForwardViewModel, client: ArcBoxClient) async throws -> URL
    {
        guard machine.isRunning, !forward.udp, forward.hostPort != 0 else {
            throw DesktopClipboard.Failure(message: "Start the machine and add a TCP VNC forward first.")
        }
        try await start()
        if let existing = sessions.values.first(where: { $0.machine == machine.id && $0.hostPort == forward.hostPort })
        {
            existing.lastUsed = Date()
            return URL(string: "\(origin)/#\(existing.token)")!
        }
        guard sessions.count < 32 else {
            throw DesktopClipboard.Failure(message: "Close unused browser desktop sessions before opening another.")
        }
        let session = WebDesktopSession(
            machine: machine.id, name: machine.name, hostPort: forward.hostPort,
            guestPort: forward.guestPort, client: client
        )
        let options = NWProtocolWebSocket.Options()
        options.autoReplyPing = true
        options.maximumMessageSize = 1024 * 1024
        let expectedOrigin = origin
        let expectedProtocol = "arcbox-" + session.token
        options.setClientRequestHandler(.main) { protocols, headers in
            let origins = headers.filter { $0.name.lowercased() == "origin" }
            let normalized = protocols.map { $0.trimmingCharacters(in: .whitespaces) }
            let valid =
                normalized.contains(expectedProtocol) && normalized.contains("binary")
                && origins.count == 1 && origins[0].value == expectedOrigin
            return NWProtocolWebSocket.Response(status: valid ? .accept : .reject, subprotocol: valid ? "binary" : nil)
        }
        let parameters = Self.loopbackParameters()
        parameters.defaultProtocolStack.applicationProtocols.insert(options, at: 0)
        let websocket = try NWListener(using: parameters)
        session.websocket = websocket
        websocket.newConnectionHandler = { [weak session] connection in
            Task { @MainActor in
                guard let session, session.relays.count < 4 else { connection.cancel(); return }
                let id = UUID()
                let relay = WebDesktopRelay(browser: connection, port: session.hostPort) { [weak session] in
                    session?.relays.removeValue(forKey: id)
                }
                session.relays[id] = relay
                relay.start()
            }
        }
        session.websocketPort = try await Self.startListener(websocket)
        sessions[session.token] = session
        return URL(string: "\(origin)/#\(session.token)")!
    }

    func stop(machine: String) {
        for (token, session) in sessions.filter({ $0.value.machine == machine }) {
            session.stop()
            sessions.removeValue(forKey: token)
            if activeToken == token { activeToken = nil }
        }
    }

    private func start() async throws {
        while starting { try await Task.sleep(for: .milliseconds(25)) }
        if listener != nil { return }
        starting = true
        defer { starting = false }
        let listener = try NWListener(using: Self.loopbackParameters())
        listener.newConnectionHandler = { [weak self] connection in
            Task { @MainActor in
                guard let self, self.connections.count < 64 else { connection.cancel(); return }
                let id = UUID()
                let handler = WebDesktopHTTPConnection(
                    connection: connection,
                    handler: { [weak self] request in
                        guard let self else { return Data() }
                        return await self.handle(request)
                    }, onClose: { [weak self] in self?.connections.removeValue(forKey: id) })
                self.connections[id] = handler
                handler.start()
            }
        }
        port = try await Self.startListener(listener)
        self.listener = listener
        maintenance = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(60))
                guard let self, !Task.isCancelled else { return }
                let expired = self.sessions.filter { Date().timeIntervalSince($0.value.lastUsed) > 1800 }
                for (token, session) in expired {
                    session.stop()
                    self.sessions.removeValue(forKey: token)
                }
            }
        }
    }

    private static func loopbackParameters() -> NWParameters {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        parameters.allowLocalEndpointReuse = false
        return parameters
    }

    private static func startListener(_ listener: NWListener) async throws -> UInt16 {
        listener.start(queue: .main)
        do {
            for _ in 0..<100 {
                switch listener.state {
                case .ready:
                    if let port = listener.port { return port.rawValue }
                case .failed(let error): throw error
                case .cancelled: throw CancellationError()
                default: break
                }
                try await Task.sleep(for: .milliseconds(50))
            }
            throw DesktopClipboard.Failure(message: "The browser desktop listener did not start in time.")
        } catch { listener.cancel(); throw error }
    }

    private func response(_ status: Int, _ body: Data, type: String = "application/json", ws: UInt16 = 0) -> Data {
        WebDesktopHTTP.response(status: status, type: type, body: body, websocketPort: ws)
    }

    private func json(_ value: [String: Any], status: Int = 200) -> Data {
        response(status, (try? JSONSerialization.data(withJSONObject: value)) ?? Data())
    }

    private func handle(_ request: WebDesktopHTTP.Request) async -> Data {
        guard request.headers["host"] == "127.0.0.1:\(port)" else {
            return json(["error": "Invalid host"], status: 403)
        }
        if let origin = request.headers["origin"], origin != self.origin {
            return json(["error": "Invalid origin"], status: 403)
        }
        if request.path.hasPrefix("/api/") {
            guard let authorization = request.headers["authorization"], authorization.hasPrefix("Bearer "),
                let session = sessions[String(authorization.dropFirst(7))]
            else { return json(["error": "Session expired. Open the desktop again from ArcBox."], status: 401) }
            session.lastUsed = Date()
            return await api(request, session: session)
        }
        guard request.method == "GET", request.body.isEmpty,
            let root = Bundle.main.resourceURL?.appendingPathComponent("WebDesktop"),
            let url = WebDesktopHTTP.assetURL(path: request.path, root: root),
            let body = try? Data(contentsOf: url), body.count <= 4 * 1024 * 1024
        else { return json(["error": "Not found"], status: 404) }
        let types = [
            "html": "text/html;charset=utf-8", "js": "text/javascript;charset=utf-8", "css": "text/css;charset=utf-8",
        ]
        // Each session owns a separate WebSocket listener; the page fetches its port after authentication.
        // CSP permits only loopback WebSockets, while their upgrade checks exact Origin + session token.
        var result = response(200, body, type: types[url.pathExtension] ?? "text/plain", ws: 0)
        if url.pathExtension == "html", let text = String(data: result, encoding: .utf8) {
            result = Data(text.replacingOccurrences(of: "ws://127.0.0.1:0", with: "ws://127.0.0.1:*").utf8)
        }
        return result
    }

    private func api(_ request: WebDesktopHTTP.Request, session: WebDesktopSession) async -> Data {
        if request.path == "/api/config", request.method == "GET" {
            return json(["name": session.name, "port": session.websocketPort, "display": session.display])
        }
        guard request.method == "POST", request.headers["origin"] == origin,
            request.headers["content-type"] == "application/json"
        else { return json(["error": "Invalid request"], status: 403) }
        if request.path == "/api/network" {
            guard !session.networkBusy else { return json(["error": "网络检测正在进行中。"], status: 409) }
            session.networkBusy = true
            defer { session.networkBusy = false }
            do { return json(["output": try await session.checkNetwork()]) } catch {
                return json(["error": error.localizedDescription], status: 422)
            }
        }
        if request.path == "/api/activate" { activeToken = session.token; return json(["ok": true]) }
        if request.path == "/api/deactivate" {
            if activeToken == session.token { activeToken = nil }
            return json(["ok": true])
        }
        if request.path == "/api/close" {
            session.stop()
            sessions.removeValue(forKey: session.token)
            if activeToken == session.token { activeToken = nil }
            return json(["ok": true])
        }
        guard activeToken == session.token else {
            return json(["error": "Focus this desktop to share its clipboard."], status: 409)
        }
        guard !session.busy else { return json(["error": "Clipboard is busy. Try again."], status: 409) }
        session.busy = true
        defer { session.busy = false }
        do {
            switch request.path {
            case "/api/settings":
                guard let object = try JSONSerialization.jsonObject(with: request.body) as? [String: Any],
                    let display = object["display"] as? Int, (0...99).contains(display),
                    let enabled = object["clipboard"] as? Bool
                else { return json(["error": "Invalid display or clipboard setting"], status: 400) }
                session.clipboardEnabled = enabled
                if session.display != UInt16(display) { session.resetClipboard(display: UInt16(display)) }
            case "/api/clipboard/paste": try await session.paste()
            case "/api/clipboard/copy": try await session.copy { self.activeToken == session.token }
            case "/api/clipboard/sync":
                try await session.synchronize { self.activeToken == session.token }
            default: return json(["error": "Not found"], status: 404)
            }
            return json(["ok": true])
        } catch { return json(["error": error.localizedDescription], status: 422) }
    }
}

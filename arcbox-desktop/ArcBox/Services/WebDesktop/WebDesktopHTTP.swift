import Foundation
import Network

/// Small, bounded HTTP/1.1 reader. Every response closes its connection.
enum WebDesktopHTTP {
    nonisolated static let maxHeaderBytes = 16 * 1024
    nonisolated static let maxBodyBytes = 8 * 1024

    nonisolated struct Request: Sendable {
        let method: String
        let path: String
        let headers: [String: String]
        let body: Data
    }

    nonisolated enum ParseResult: Sendable {
        case incomplete
        case invalid
        case request(Request)
    }

    nonisolated static func parse(_ data: Data) -> ParseResult {
        guard let end = data.range(of: Data("\r\n\r\n".utf8)) else {
            return data.count > maxHeaderBytes ? .invalid : .incomplete
        }
        guard end.upperBound <= maxHeaderBytes,
            let header = String(data: data[..<end.lowerBound], encoding: .utf8)
        else { return .invalid }
        let lines = header.components(separatedBy: "\r\n")
        let first = lines[0].split(separator: " ")
        guard first.count == 3, first[2] == "HTTP/1.1", first[1].hasPrefix("/") else { return .invalid }
        var headers: [String: String] = [:]
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":"), !line.hasPrefix(" "), !line.hasPrefix("\t") else {
                return .invalid
            }
            let name = line[..<colon].lowercased()
            guard headers[name] == nil else { return .invalid }
            headers[name] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        guard headers["transfer-encoding"] == nil else { return .invalid }
        let length: Int
        if let raw = headers["content-length"] {
            guard !raw.isEmpty, raw.allSatisfy({ $0.isASCII && $0.isNumber }), let count = Int(raw),
                count <= maxBodyBytes
            else { return .invalid }
            length = count
        } else {
            length = 0
        }
        guard data.count >= end.upperBound + length else { return .incomplete }
        guard data.count == end.upperBound + length else { return .invalid }
        return .request(
            Request(
                method: String(first[0]), path: String(first[1]), headers: headers,
                body: Data(data[end.upperBound...])
            ))
    }

    nonisolated static func assetURL(path: String, root: URL) -> URL? {
        let path = path == "/" ? "/index.html" : path
        guard !path.contains("%"), !path.contains("\\"), !path.contains("?"),
            !path.split(separator: "/").contains("..")
        else { return nil }
        let file = root.appendingPathComponent(String(path.dropFirst())).standardizedFileURL
        guard file.path.hasPrefix(root.standardizedFileURL.path + "/") else { return nil }
        return file
    }

    nonisolated static func response(status: Int, type: String, body: Data, websocketPort: UInt16) -> Data {
        let reason = status == 200 ? "OK" : "Request Failed"
        let policy =
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; "
            + "connect-src 'self' ws://127.0.0.1:\(websocketPort); frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
        let headers =
            "HTTP/1.1 \(status) \(reason)\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\n"
            + "Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n"
            + "Referrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n"
            + "Content-Security-Policy: \(policy)\r\n\r\n"
        return Data(headers.utf8) + body
    }
}

@MainActor
final class WebDesktopHTTPConnection {
    let connection: NWConnection
    private var buffer = Data()
    private var timeout: Task<Void, Never>?
    private let handler: (WebDesktopHTTP.Request) async -> Data
    private let onClose: () -> Void
    private var closed = false

    init(
        connection: NWConnection, handler: @escaping (WebDesktopHTTP.Request) async -> Data,
        onClose: @escaping () -> Void
    ) {
        self.connection = connection
        self.handler = handler
        self.onClose = onClose
    }

    func start() {
        connection.start(queue: .main)
        timeout = Task { [weak self] in
            try? await Task.sleep(for: .seconds(20))
            if !Task.isCancelled { self?.close() }
        }
        receive()
    }

    func close() {
        guard !closed else { return }
        closed = true
        timeout?.cancel()
        connection.cancel()
        onClose()
    }

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16 * 1024) { [weak self] data, _, eof, error in
            Task { @MainActor in
                guard let self, !self.closed else { return }
                if let data { self.buffer.append(data) }
                switch WebDesktopHTTP.parse(self.buffer) {
                case .invalid: self.close()
                case .incomplete:
                    if eof || error != nil { self.close() } else { self.receive() }
                case .request(let request):
                    let response = await self.handler(request)
                    guard !self.closed else { return }
                    self.connection.send(
                        content: response,
                        completion: .contentProcessed { [weak self] _ in
                            Task { @MainActor in self?.close() }
                        })
                }
            }
        }
    }
}

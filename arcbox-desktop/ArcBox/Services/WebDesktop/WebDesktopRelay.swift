import Foundation
import Network

/// The platform WebSocket parser handles masking, fragmentation, ping/pong and size limits.
@MainActor
final class WebDesktopRelay {
    private let browser: NWConnection
    private let vnc: NWConnection
    private let onClose: () -> Void
    private var closed = false
    private var startupTimeout: Task<Void, Never>?

    init(browser: NWConnection, port: UInt16, onClose: @escaping () -> Void) {
        self.browser = browser
        self.vnc = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        self.onClose = onClose
    }

    func start() {
        startupTimeout = Task { [weak self] in
            try? await Task.sleep(for: .seconds(10))
            if !Task.isCancelled { self?.close() }
        }
        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                guard let self else { return }
                switch state {
                case .ready: self.vnc.start(queue: .main)
                case .failed, .cancelled: self.close()
                default: break
                }
            }
        }
        vnc.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                guard let self else { return }
                switch state {
                case .ready:
                    self.startupTimeout?.cancel()
                    self.readBrowser()
                    self.readVNC()
                case .failed, .cancelled: self.close()
                default: break
                }
            }
        }
        browser.start(queue: .main)
    }

    func close() {
        guard !closed else { return }
        closed = true
        startupTimeout?.cancel()
        browser.cancel()
        vnc.cancel()
        onClose()
    }

    private func readBrowser() {
        browser.receiveMessage { [weak self] data, context, _, error in
            Task { @MainActor in
                guard let self, !self.closed else { return }
                guard error == nil,
                    let metadata = context?.protocolMetadata(definition: NWProtocolWebSocket.definition)
                        as? NWProtocolWebSocket.Metadata,
                    metadata.opcode != .close
                else { self.close(); return }
                guard metadata.opcode == .binary, let data else {
                    if metadata.opcode == .ping || metadata.opcode == .pong { self.readBrowser() } else { self.close() }
                    return
                }
                self.vnc.send(
                    content: data,
                    completion: .contentProcessed { [weak self] error in
                        Task { @MainActor in
                            if error != nil { self?.close() } else { self?.readBrowser() }
                        }
                    })
            }
        }
    }

    private func readVNC() {
        vnc.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, eof, error in
            Task { @MainActor in
                guard let self, !self.closed else { return }
                guard let data, !data.isEmpty, error == nil else { self.close(); return }
                let context = NWConnection.ContentContext(
                    identifier: "vnc", metadata: [NWProtocolWebSocket.Metadata(opcode: .binary)]
                )
                self.browser.send(
                    content: data, contentContext: context,
                    completion: .contentProcessed { [weak self] error in
                        Task { @MainActor in
                            if error != nil || eof { self?.close() } else { self?.readVNC() }
                        }
                    })
            }
        }
    }
}

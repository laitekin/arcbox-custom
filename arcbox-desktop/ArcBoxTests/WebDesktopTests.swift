import AppKit
import ArcBoxClient
import Foundation
import XCTest

@testable import ArcBox

@MainActor
final class WebDesktopTests: XCTestCase {
    func testHTTPWaitsForCompleteHeadersAndBody() {
        let head = "POST /api/activate HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nContent-Length: 2\r\n\r\n"
        guard case .incomplete = WebDesktopHTTP.parse(Data(head.utf8)) else {
            return XCTFail("Expected incomplete body")
        }
        guard case .request(let request) = WebDesktopHTTP.parse(Data((head + "{}").utf8)) else {
            return XCTFail("Expected complete request")
        }
        XCTAssertEqual(request.method, "POST")
        XCTAssertEqual(request.path, "/api/activate")
        XCTAssertEqual(request.headers["host"], "127.0.0.1:1234")
        XCTAssertEqual(request.body, Data("{}".utf8))
    }

    func testHTTPRejectsAmbiguousOrOversizedRequests() {
        let invalid = [
            "POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 2\r\ncontent-length: 2\r\n\r\n{}",
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\nGET / HTTP/1.1\r\n\r\n",
            String(repeating: "a", count: 17000),
        ]
        for request in invalid {
            guard case .invalid = WebDesktopHTTP.parse(Data(request.utf8)) else {
                XCTFail("Accepted malformed request"); continue
            }
        }
    }

    func testAssetsCannotEscapeTheResourceRoot() {
        let root = URL(fileURLWithPath: "/tmp/WebDesktop", isDirectory: true)
        for path in ["/../secret", "/%2e%2e/secret", "/vendor/../../secret", "/\\secret", "/foo?bar"] {
            XCTAssertNil(WebDesktopHTTP.assetURL(path: path, root: root))
        }
        XCTAssertEqual(WebDesktopHTTP.assetURL(path: "/", root: root)?.lastPathComponent, "index.html")
        XCTAssertEqual(
            WebDesktopHTTP.assetURL(path: "/vendor/core/rfb.js", root: root)?.path,
            "/tmp/WebDesktop/vendor/core/rfb.js")
    }

    func testClipboardUnicodeAndImageRoundTripThroughNamedPasteboard() throws {
        let board = NSPasteboard(name: .init("ArcBox.WebDesktopTests.\(UUID())"))
        defer { board.releaseGlobally() }
        let text = DesktopClipboard.Payload(mime: "text/plain;charset=utf-8", data: Data("中文🙂\nhello".utf8))
        try DesktopClipboard.writeMac(text, board: board)
        XCTAssertEqual(try DesktopClipboard.readMac(board), text)
        let bitmap = try XCTUnwrap(
            NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: 2, pixelsHigh: 2,
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                bytesPerRow: 8, bitsPerPixel: 32))
        bitmap.setColor(NSColor(deviceRed: 0, green: 0, blue: 1, alpha: 1), atX: 0, y: 0)
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        let image = DesktopClipboard.Payload(mime: "image/png", data: png)
        try DesktopClipboard.writeMac(image, board: board)
        XCTAssertEqual(try DesktopClipboard.readMac(board), image)
        board.clearContents()
        board.setData(try XCTUnwrap(bitmap.tiffRepresentation), forType: .tiff)
        XCTAssertEqual(try DesktopClipboard.readMac(board)?.mime, "image/png", "Mac screenshots may expose TIFF")
    }

    func testClipboardRejectsInvalidDataAndKeepsBytesOutOfArguments() throws {
        XCTAssertThrowsError(try DesktopClipboard.validate(.init(mime: "image/png", data: Data("fake".utf8))))
        XCTAssertThrowsError(try DesktopClipboard.validate(.init(mime: "text/html", data: Data())))
        XCTAssertThrowsError(try DesktopClipboard.validate(.init(mime: "text/plain;charset=utf-8", data: Data([255]))))
        XCTAssertThrowsError(
            try DesktopClipboard.validate(
                .init(
                    mime: "text/plain;charset=utf-8",
                    data: Data(repeating: 65, count: DesktopClipboard.maxBytes + 1))))
        let request = DesktopClipboard.request(machine: "test", display: 7, mode: "set", mime: "image/png")
        XCTAssertTrue(request.attachStdin)
        XCTAssertEqual(Array(request.cmd.suffix(3)), ["set", "7", "image/png"])
        XCTAssertTrue(request.env.isEmpty)
        let focus = DesktopClipboard.request(machine: "test", display: 1, mode: "focus")
        XCTAssertFalse(focus.attachStdin)
        XCTAssertEqual(Array(focus.cmd.suffix(3)), ["focus", "1", ""])
    }

    func testGatewayServesBundledAssetsAndRequiresSession() async throws {
        let client = try ArcBoxClient(socketPath: "/tmp/arcbox-web-no-daemon.sock")
        defer { client.close() }
        var summary = Arcbox_V1_MachineSummary()
        summary.id = "web-test"; summary.name = "Web Test"
        var machine = MachineViewModel(from: summary)
        machine.state = .running
        let server = WebDesktopServer()
        let url = try await server.open(
            machine: machine,
            forward: .init(hostPort: 5901, guestPort: 5901), client: client)
        defer { server.stop(machine: machine.id) }
        let pageURL = try XCTUnwrap(URL(string: server.origin + "/"))
        let (page, response) = try await URLSession.shared.data(from: pageURL)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertTrue(String(decoding: page, as: UTF8.self).contains("远程桌面键盘输入"))
        XCTAssertTrue(
            (response as? HTTPURLResponse)?.value(forHTTPHeaderField: "Content-Security-Policy")?
                .contains("frame-ancestors 'none'") == true)
        let configURL = try XCTUnwrap(URL(string: server.origin + "/api/config"))
        let (_, denied) = try await URLSession.shared.data(from: configURL)
        XCTAssertEqual((denied as? HTTPURLResponse)?.statusCode, 401)
        var request = URLRequest(url: configURL)
        request.setValue("Bearer " + (url.fragment ?? ""), forHTTPHeaderField: "Authorization")
        let (data, authorized) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((authorized as? HTTPURLResponse)?.statusCode, 200)
        let config = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(config["name"] as? String, "Web Test")
        XCTAssertEqual(config["display"] as? Int, 1)
        request.setValue("https://example.com", forHTTPHeaderField: "Origin")
        let (_, crossOrigin) = try await URLSession.shared.data(for: request)
        XCTAssertEqual((crossOrigin as? HTTPURLResponse)?.statusCode, 403)
    }

    /// Explicit opt-in against a running X11 guest, restoring its previous clipboard afterward.
    func testLiveClipboardTextImageAndWebSocket() async throws {
        let env = ProcessInfo.processInfo.environment
        guard let machineID = env["WEB_DESKTOP_TEST_MACHINE"] else {
            throw XCTSkip("Set TEST_RUNNER_WEB_DESKTOP_TEST_MACHINE")
        }
        let socket = env["WEB_DESKTOP_TEST_SOCKET"] ?? NSHomeDirectory() + "/.arcbox-dev/run/arcbox.sock"
        let client = try ArcBoxClient(socketPath: socket)
        let task = Task { try await client.runConnections() }
        defer { client.close(); task.cancel() }
        let old = try await DesktopClipboard.read(machine: machineID, display: 1, client: client)
        let networkSession = WebDesktopSession(
            machine: machineID, name: machineID, hostPort: 5901,
            guestPort: 5901, client: client)
        let network = try await networkSession.checkNetwork()
        XCTAssertTrue(network.contains("检测位置：当前虚拟机"))
        XCTAssertTrue(network.contains("HTTPS"))
        print("NETWORK CHECK: " + network)

        do {
            let text = DesktopClipboard.Payload(
                mime: "text/plain;charset=utf-8",
                data: Data(("ArcBox 中文输入🙂\n" + String(repeating: "大", count: 90000)).utf8))
            try await DesktopClipboard.write(text, machine: machineID, display: 1, client: client)
            let returnedText = try await DesktopClipboard.read(machine: machineID, display: 1, client: client)
            XCTAssertEqual(returnedText, text)
            let bitmap = try XCTUnwrap(
                NSBitmapImageRep(
                    bitmapDataPlanes: nil, pixelsWide: 16, pixelsHigh: 16,
                    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                    bytesPerRow: 64, bitsPerPixel: 32))
            bitmap.setColor(NSColor(deviceRed: 1, green: 0, blue: 0, alpha: 1), atX: 0, y: 0)
            let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
            let image = DesktopClipboard.Payload(mime: "image/png", data: png)
            try await DesktopClipboard.write(image, machine: machineID, display: 1, client: client)
            let returnedImage = try await DesktopClipboard.read(machine: machineID, display: 1, client: client)
            XCTAssertEqual(returnedImage, image)
            // Whatever has focus in the guest, the probe answers instead of failing.
            _ = try await DesktopClipboard.focusIsTerminal(machine: machineID, display: 1, client: client)
            if let old { try await DesktopClipboard.write(old, machine: machineID, display: 1, client: client) }
        } catch {
            if let old { try? await DesktopClipboard.write(old, machine: machineID, display: 1, client: client) }
            throw error
        }
        var summary = Arcbox_V1_MachineSummary()
        summary.id = machineID; summary.name = machineID
        var machine = MachineViewModel(from: summary)
        machine.state = .running
        let server = WebDesktopServer()
        let url = try await server.open(
            machine: machine, forward: .init(hostPort: 5901, guestPort: 5901), client: client)
        defer { server.stop(machine: machineID) }
        var configRequest = URLRequest(url: try XCTUnwrap(URL(string: server.origin + "/api/config")))
        configRequest.setValue("Bearer " + (url.fragment ?? ""), forHTTPHeaderField: "Authorization")
        let (configData, _) = try await URLSession.shared.data(for: configRequest)
        let config = try XCTUnwrap(JSONSerialization.jsonObject(with: configData) as? [String: Any])
        let port = try XCTUnwrap(config["port"] as? Int)
        var request = URLRequest(url: try XCTUnwrap(URL(string: "ws://127.0.0.1:\(port)/")))
        request.timeoutInterval = 10
        request.setValue(server.origin, forHTTPHeaderField: "Origin")
        request.setValue("binary, arcbox-" + (url.fragment ?? ""), forHTTPHeaderField: "Sec-WebSocket-Protocol")
        let websocket = URLSession.shared.webSocketTask(with: request)
        websocket.resume()
        defer { websocket.cancel(with: .normalClosure, reason: nil) }
        let frame = try await websocket.receive()
        guard case .data(let banner) = frame else { return XCTFail("Expected binary RFB greeting") }
        XCTAssertTrue(String(decoding: banner, as: UTF8.self).hasPrefix("RFB "))
        if let path = env["WEB_DESKTOP_BROWSER_URL_FILE"] {
            try url.absoluteString.write(toFile: path, atomically: true, encoding: .utf8)
            try await Task.sleep(for: .seconds(240))
        }

    }
}

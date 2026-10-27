import AppKit
import ArcBoxClient
import Foundation
import GRPCCore
import Network

@MainActor
final class WebDesktopSession {
    let machine: String
    let name: String
    let hostPort: UInt16
    let client: ArcBoxClient
    let token =
        UUID().uuidString.replacingOccurrences(of: "-", with: "")
        + UUID().uuidString.replacingOccurrences(of: "-", with: "")
    var display: UInt16
    var websocket: NWListener?
    var websocketPort: UInt16 = 0
    var relays: [UUID: WebDesktopRelay] = [:]
    var lastUsed = Date()
    var busy = false
    var networkBusy = false
    var clipboardEnabled = true
    private var lastMacChange = -1
    private var lastGuest: String?

    init(machine: String, name: String, hostPort: UInt16, guestPort: UInt16, client: ArcBoxClient) {
        self.machine = machine
        self.name = name
        self.hostPort = hostPort
        self.display = guestPort >= 5900 && guestPort <= 5999 ? guestPort - 5900 : 0
        self.client = client
    }

    func resetClipboard(display: UInt16) {
        self.display = display
        lastMacChange = -1
        lastGuest = nil
    }

    func stop() {
        websocket?.cancel()
        for relay in Array(relays.values) { relay.close() }
        relays.removeAll()
    }

    /// Puts the Mac clipboard on the machine's before a paste. When the Mac clipboard has
    /// not changed since it last went either way, the machine's own clipboard is the newest
    /// and stays, as when copying and pasting within one Mac app.
    func paste() async throws {
        let change = NSPasteboard.general.changeCount
        guard change != lastMacChange else { return }
        guard let payload = try DesktopClipboard.readMac() else {
            lastMacChange = change
            return
        }
        try await DesktopClipboard.write(payload, machine: machine, display: display, client: client)
        lastGuest = payload.fingerprint
        lastMacChange = change
    }

    func copy(isActive: () -> Bool) async throws {
        let initialMacChange = NSPasteboard.general.changeCount
        var payload: DesktopClipboard.Payload?
        // X11 selection ownership can lag behind the key event.
        for _ in 0..<6 {
            payload = try await DesktopClipboard.read(machine: machine, display: display, client: client)
            if let payload, payload.fingerprint != lastGuest { break }
            try await Task.sleep(for: .milliseconds(120))
        }
        // Nothing selected: like ⌘C on the Mac, leave the clipboard alone.
        guard let payload else { return }
        guard isActive(), NSPasteboard.general.changeCount == initialMacChange else { return }
        try DesktopClipboard.writeMac(payload)
        lastMacChange = NSPasteboard.general.changeCount
        lastGuest = payload.fingerprint
    }

    func focusIsTerminal() async -> Bool {
        (try? await DesktopClipboard.focusIsTerminal(machine: machine, display: display, client: client)) == true
    }

    func synchronize(isActive: () -> Bool) async throws {
        guard clipboardEnabled, isActive() else { return }
        if NSPasteboard.general.changeCount != lastMacChange {
            if try DesktopClipboard.readMac() != nil {
                try await paste()
            } else {
                lastMacChange = NSPasteboard.general.changeCount
            }
            return
        }
        let change = NSPasteboard.general.changeCount
        let payload = try await DesktopClipboard.read(machine: machine, display: display, client: client)
        guard isActive(), NSPasteboard.general.changeCount == change, let payload else { return }
        defer { lastGuest = payload.fingerprint }
        guard let lastGuest, lastGuest != payload.fingerprint else { return }
        try DesktopClipboard.writeMac(payload)
        lastMacChange = NSPasteboard.general.changeCount
    }
}

extension WebDesktopSession {
    /// Fixed destinations, executed by this machine's agent through its configured network.
    static let networkScript = #"""
        printf '检测位置：当前虚拟机\n\n'
        if command -v timeout >/dev/null 2>&1 && command -v getent >/dev/null 2>&1; then
          dns=$(timeout 4 getent ahostsv4 example.com 2>/dev/null | head -n 1)
          if [ -n "$dns" ]; then printf 'DNS：正常  %s\n' "$dns"
          else printf 'DNS：未取得结果（超时或解析失败）\n'; fi
        else printf 'DNS：未检测（缺少 getent / timeout）\n'; fi
        if ! command -v curl >/dev/null 2>&1; then
          printf '\nHTTPS / 出口 IP：未检测（请在机器内安装 curl）\n'; exit 0
        fi
        trace=$(curl -fsS --connect-timeout 3 --max-time 6 --max-filesize 8192 https://www.cloudflare.com/cdn-cgi/trace 2>/dev/null)
        if [ $? -eq 0 ]; then
          printf '\n出口 IP：%s\n' "$(printf '%s\n' "$trace" | sed -n 's/^ip=//p')"
          printf '出口地区：%s\n' "$(printf '%s\n' "$trace" | sed -n 's/^loc=//p')"
          printf 'Cloudflare HTTPS：正常\n'
        else printf '\n出口 IP / Cloudflare HTTPS：连接失败或超时\n'; fi
        result=$(curl -sS --connect-timeout 3 --max-time 6 -o /dev/null -w '%{http_code} / %{time_total}s' https://www.google.com/generate_204 2>/dev/null)
        if [ $? -eq 0 ]; then printf 'Google HTTPS：%s\n' "$result"
        else printf 'Google HTTPS：连接失败或超时\n'; fi
        printf '\n仅反映本次检测；浏览器自己的代理、DNS 或 WebRTC 配置可能不同。\n'
        """#

    func checkNetwork() async throws -> String {
        var request = Arcbox_V1_MachineExecRequest()
        request.id = machine
        request.cmd = ["/bin/sh", "-c", Self.networkScript]
        var options = ArcBoxClient.defaultCallOptions
        options.timeout = .seconds(18)
        let output = try await MachineExec.run(request, client: client, options: options, maxOutputBytes: 16 * 1024)
        guard output.exitCode == 0 else {
            throw DesktopClipboard.Failure(message: "网络检测未完成，请确认机器正在运行。")
        }
        return String(decoding: output.stdout, as: UTF8.self)
    }
}

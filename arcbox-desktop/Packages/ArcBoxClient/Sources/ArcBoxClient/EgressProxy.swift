import Foundation

/// Where guest egress goes, mirroring the proto `EgressProxyMode`.
public enum EgressProxyMode: String, CaseIterable, Identifiable, Sendable {
    /// Follow the Mac's proxy settings.
    case system
    /// Always connect directly.
    case direct
    /// Use a proxy entered here.
    case custom

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .system: "Follow macOS"
        case .direct: "Direct"
        case .custom: "Custom proxy"
        }
    }
}

/// The protocol of a custom proxy.
public enum EgressProxyProtocol: String, CaseIterable, Identifiable, Sendable {
    case socks5
    case http

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .socks5: "SOCKS5"
        case .http: "HTTP"
        }
    }

    var defaultPort: Int {
        switch self {
        case .socks5: 1080
        case .http: 3128
        }
    }
}

/// What happens to guest UDP, mirroring the proto `EgressUdpPolicy`.
public enum EgressUdpPolicy: String, CaseIterable, Identifiable, Sendable {
    case proxy
    case direct
    case block

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .proxy: "Through the proxy"
        case .direct: "Direct"
        case .block: "Block"
        }
    }

    init(proto: Arcbox_V1_EgressUdpPolicy) {
        switch proto {
        case .direct: self = .direct
        case .block: self = .block
        case .proxy, .unspecified, .UNRECOGNIZED: self = .proxy
        }
    }

    var proto: Arcbox_V1_EgressUdpPolicy {
        switch self {
        case .proxy: .proxy
        case .direct: .direct
        case .block: .block
        }
    }
}

/// What happens to guest ICMP echo, mirroring the proto `EgressIcmpPolicy`.
public enum EgressIcmpPolicy: String, CaseIterable, Identifiable, Sendable {
    case direct
    case block

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .direct: "Direct"
        case .block: "Block"
        }
    }

    init(proto: Arcbox_V1_EgressIcmpPolicy) {
        switch proto {
        case .block: self = .block
        case .direct, .unspecified, .UNRECOGNIZED: self = .direct
        }
    }

    var proto: Arcbox_V1_EgressIcmpPolicy {
        switch self {
        case .direct: .direct
        case .block: .block
        }
    }
}

/// A change to the stored proxy password.
public enum EgressProxyPasswordChange: Equatable, Sendable {
    /// Leave the stored password as it is.
    case keep
    /// Store this password.
    case set(String)
    /// Remove the stored password.
    case clear
}

/// The egress proxy settings as an editable value. Never holds a password:
/// the daemon only reports whether one is stored.
public struct EgressProxySettings: Equatable, Sendable {
    public var mode: EgressProxyMode = .system
    public var proxyProtocol: EgressProxyProtocol = .socks5
    public var host = ""
    public var port = ""
    public var username = ""
    /// Hosts that bypass the proxy, one per entry.
    public var exclude: [String] = []
    public var udp: EgressUdpPolicy = .proxy
    public var icmp: EgressIcmpPolicy = .direct

    /// Output only: a password is stored for the custom proxy's user.
    public private(set) var hasPassword = false
    /// Output only: the proxies flows use right now (empty: direct).
    public private(set) var effective: [String] = []
    /// Output only: where the password lives (`keychain`, `file`).
    public private(set) var credentialStore = ""

    public init() {}

    /// The settings a daemon reported.
    public init(proto: Arcbox_V1_EgressProxySettings) {
        switch proto.mode {
        case .none: mode = .direct
        case .custom: mode = .custom
        case .system, .unspecified, .UNRECOGNIZED: mode = .system
        }
        if mode == .custom, let parsed = Self.parse(url: proto.url) {
            proxyProtocol = parsed.proxyProtocol
            host = parsed.host
            port = parsed.port
            username = parsed.username
        }
        exclude = proto.exclude
        udp = EgressUdpPolicy(proto: proto.udp)
        icmp = EgressIcmpPolicy(proto: proto.icmp)
        hasPassword = proto.hasPassword_p
        effective = proto.effective
        credentialStore = proto.credentialStore
    }

    /// Whether `other` describes the same proxy account (so a stored
    /// password still applies to it).
    public func sameAccount(as other: EgressProxySettings) -> Bool {
        mode == .custom && other.mode == .custom && proxyProtocol == other.proxyProtocol
            && host == other.host && port == other.port && username == other.username
    }

    /// Why these settings cannot be saved, if they cannot.
    public var validationError: String? {
        guard mode == .custom else { return nil }
        let trimmedHost = host.trimmingCharacters(in: .whitespaces)
        if trimmedHost.isEmpty {
            return "Enter the proxy server's address."
        }
        if trimmedHost.contains(where: { $0.isWhitespace || "/?#@".contains($0) }) {
            return "The server address must be a host name or an IP address."
        }
        guard let portNumber = Int(port.trimmingCharacters(in: .whitespaces)),
            (1...65535).contains(portNumber)
        else {
            return "Enter a port between 1 and 65535."
        }
        if username.utf8.count > 255 {
            return "The user name is longer than 255 bytes."
        }
        if proxyProtocol == .http, username.contains(":") {
            return "An HTTP proxy user name cannot contain a colon."
        }
        return nil
    }

    /// The password-free proxy URL (`socks5://user@host:port`).
    public var url: String {
        let scheme = proxyProtocol.rawValue
        let trimmedHost = host.trimmingCharacters(in: .whitespaces)
        let hostPart = trimmedHost.contains(":") ? "[\(trimmedHost)]" : trimmedHost
        let user =
            username.isEmpty
            ? ""
            : (username.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? username) + "@"
        return "\(scheme)://\(user)\(hostPart):\(port.trimmingCharacters(in: .whitespaces))"
    }

    /// The wire settings; output-only fields are left unset.
    public var proto: Arcbox_V1_EgressProxySettings {
        var settings = Arcbox_V1_EgressProxySettings()
        switch mode {
        case .system: settings.mode = .system
        case .direct: settings.mode = .none
        case .custom:
            settings.mode = .custom
            settings.url = url
        }
        settings.exclude = exclude.map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        settings.udp = udp.proto
        settings.icmp = icmp.proto
        return settings
    }

    /// A `SetProxy` request for these settings and `password`.
    public func setRequest(password: EgressProxyPasswordChange) -> Arcbox_V1_SetEgressProxyRequest {
        var request = Arcbox_V1_SetEgressProxyRequest()
        request.settings = proto
        switch password {
        case .keep: break
        case .set(let value): request.password = value
        case .clear: request.password = ""
        }
        return request
    }

    /// Splits `scheme://[user@]host:port` (IPv6 in brackets).
    static func parse(url: String) -> (
        proxyProtocol: EgressProxyProtocol, host: String, port: String, username: String
    )? {
        guard let schemeEnd = url.range(of: "://") else { return nil }
        let scheme = url[..<schemeEnd.lowerBound].lowercased()
        let proxyProtocol: EgressProxyProtocol =
            scheme.hasPrefix("socks") ? .socks5 : .http
        var rest = Substring(url[schemeEnd.upperBound...])
        while rest.hasSuffix("/") { rest = rest.dropLast() }
        var username = ""
        if let at = rest.lastIndex(of: "@") {
            let userinfo = rest[..<at]
            let user = userinfo.split(separator: ":", maxSplits: 1).first.map(String.init) ?? ""
            username = user.removingPercentEncoding ?? user
            rest = rest[rest.index(after: at)...]
        }
        guard let colon = rest.lastIndex(of: ":") else { return nil }
        var host = String(rest[..<colon])
        if host.hasPrefix("["), host.hasSuffix("]") {
            host = String(host.dropFirst().dropLast())
        }
        let port = String(rest[rest.index(after: colon)...])
        return (proxyProtocol, host, port, username)
    }
}

/// One finished step of a proxy test.
public struct EgressProxyTestStep: Identifiable, Equatable, Sendable {
    public enum Kind: String, Sendable {
        case select, reach, authenticate, tunnel, udp

        public var title: String {
            switch self {
            case .select: "Proxy"
            case .reach: "Connect to proxy"
            case .authenticate: "Authenticate"
            case .tunnel: "Reach target"
            case .udp: "UDP relay"
            }
        }
    }

    public var id: String { kind.rawValue }
    public let kind: Kind
    public let ok: Bool
    public let skipped: Bool
    public let latencyMilliseconds: UInt64
    /// Why the step failed, in words; `nil` when it did not.
    public let failure: String?
    public let message: String

    public init(proto: Arcbox_V1_EgressProxyTestStep) {
        switch proto.step {
        case .reach: kind = .reach
        case .authenticate: kind = .authenticate
        case .tunnel: kind = .tunnel
        case .udp: kind = .udp
        case .select, .unspecified, .UNRECOGNIZED: kind = .select
        }
        ok = proto.ok
        skipped = proto.skipped
        latencyMilliseconds = proto.latencyMs
        message = proto.message
        failure =
            switch proto.failure {
            case .unspecified, .UNRECOGNIZED: nil
            case .unreachable: "The proxy is unreachable"
            case .timeout: "Timed out"
            case .authRejected: "The user name or password was rejected"
            case .noAcceptableMethod: "The proxy does not accept the configured authentication"
            case .missingPassword: "No password is stored"
            case .targetRefused: "The proxy refused the target"
            case .udpUnsupported: "The proxy does not relay UDP"
            case .`protocol`: "The proxy answered unexpectedly"
            case .config: "The settings are incomplete"
            }
    }
}

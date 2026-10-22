import Foundation

/// A saved proxy returned by the daemon. Its password is never returned.
public struct SavedProxyProfile: Identifiable, Sendable {
    public let id: String
    public let name: String
    public let settings: EgressProxySettings
    public let machines: [String]
    public let isDefault: Bool

    public init(proto: Arcbox_V1_ProxyProfile) {
        id = proto.id
        name = proto.name
        settings = EgressProxySettings(proto: proto.settings)
        machines = proto.machines
        isDefault = proto.isDefault
    }

    public var isInUse: Bool { isDefault || !machines.isEmpty }
}

/// Transient import result; the password is write-only on save/test.
public struct ImportedSocksProxy {
    public var settings: EgressProxySettings
    public var password: String?
}

public enum SocksURLImport {
    public enum ImportError: LocalizedError {
        case invalid, credentials
        public var errorDescription: String? {
            switch self {
            case .invalid: "Enter a SOCKS5 URL, for example socks5h://user:password@host:1080."
            case .credentials: "Proxy credentials must use valid percent encoding and contain 1–255 UTF-8 bytes."
            }
        }
    }

    public static func parse(_ text: String) throws -> ImportedSocksProxy {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard text.utf8.count <= 4096 else { throw ImportError.invalid }
        let bytes = Array(text.utf8)
        for index in bytes.indices where bytes[index] == 37 {
            guard index + 2 < bytes.count,
                bytes[(index + 1)...(index + 2)].allSatisfy({
                    (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
                })
            else { throw ImportError.credentials }
        }
        guard let url = URLComponents(string: text),
            let scheme = url.scheme?.lowercased(), ["socks5", "socks5h", "socks"].contains(scheme),
            let host = url.host, !host.isEmpty, host.utf8.count <= 253,
            url.path.isEmpty || url.path == "/", url.query == nil, url.fragment == nil
        else { throw ImportError.invalid }
        var settings = EgressProxySettings()
        settings.mode = .custom
        settings.remoteDNS = scheme == "socks5h"
        settings.host = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        settings.port = String(url.port ?? 1080)
        let user = try decode(url.percentEncodedUser)
        let password = try decode(url.percentEncodedPassword)
        if let user {
            guard (1...255).contains(user.utf8.count) else { throw ImportError.credentials }
            settings.username = user
        }
        if let password {
            guard user != nil, (1...255).contains(password.utf8.count) else { throw ImportError.credentials }
        }
        guard settings.validationError == nil else { throw ImportError.invalid }
        return ImportedSocksProxy(settings: settings, password: password)
    }

    private static func decode(_ encoded: String?) throws -> String? {
        guard let encoded else { return nil }
        guard let decoded = encoded.removingPercentEncoding else { throw ImportError.credentials }
        return decoded
    }
}

import ArcBoxClient
import Foundation
import OSLog

/// The menu bar's "Proxy: On/Off" item. Switching off saves direct mode and
/// remembers the previous mode in `UserDefaults`; switching on restores it.
@MainActor
@Observable
final class ProxyQuickToggleModel {
    static let rememberedKey = "egressProxy.quickToggle.remembered"

    /// The daemon's settings, once loaded.
    private(set) var settings: EgressProxySettings?
    private(set) var isSaving = false
    private(set) var lastError: String?

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var isOn: Bool? { settings.map { $0.mode != .direct } }

    func load(client: ArcBoxClient?) async {
        guard let client else {
            settings = nil
            return
        }
        do {
            let reply = try await client.egressProxy.getProxy(.init(), options: ArcBoxClient.defaultCallOptions)
            settings = EgressProxySettings(proto: reply)
            lastError = nil
        } catch {
            Log.daemon.error("Failed to load proxy settings: \(error.localizedDescription, privacy: .private)")
            settings = nil
        }
    }

    func toggle(client: ArcBoxClient?) async {
        guard let client, let current = settings, !isSaving else { return }
        isSaving = true
        lastError = nil
        let next: EgressProxySettings
        if let remembered = EgressProxyQuickToggle.remembered(current) {
            defaults.set(remembered, forKey: Self.rememberedKey)
            next = EgressProxyQuickToggle.off(current)
        } else {
            next = EgressProxyQuickToggle.on(current, remembered: defaults.string(forKey: Self.rememberedKey))
        }
        do {
            let reply = try await client.egressProxy.setProxy(
                next.setRequest(password: .keep), options: ArcBoxClient.defaultCallOptions)
            settings = EgressProxySettings(proto: reply)
        } catch {
            Log.daemon.error("Failed to switch the proxy: \(error.localizedDescription, privacy: .private)")
            lastError = ArcBoxClient.userMessage(for: error)
        }
        isSaving = false
    }
}

import ArcBoxClient
import Foundation
import GRPCCore
import OSLog

/// Drives Settings > Network: the guest egress proxy, its password, and the
/// step-by-step proxy test. Edits are a draft until saved; the daemon
/// applies a save to new connections at once.
@MainActor
@Observable
final class NetworkSettingsModel {
    enum LoadState: Equatable {
        case waiting
        case loading
        case loaded
        case failed(String)
    }

    private(set) var loadState: LoadState = .waiting
    /// What the daemon reported last.
    private(set) var saved = EgressProxySettings()
    /// The settings being edited.
    var draft = EgressProxySettings()
    /// A password typed for the draft; empty means "keep the stored one".
    var password = ""
    /// Exclusions as edited, one host per line.
    var excludeText = ""

    private(set) var isSaving = false
    private(set) var saveError: String?
    private(set) var isTesting = false
    private(set) var testSteps: [EgressProxyTestStep] = []
    private(set) var testError: String?

    var isDirty: Bool {
        var edited = draft
        edited.exclude = Self.lines(excludeText)
        return edited != saved || !password.isEmpty
    }

    /// The draft needs a password it does not have: a new user, nothing typed.
    var needsPassword: Bool {
        draft.mode == .custom && draft.profileID.isEmpty && !draft.username.isEmpty && password.isEmpty
            && !(draft.sameAccount(as: saved) && saved.hasPassword)
    }

    var validationError: String? {
        if let error = draft.validationError { return error }
        if needsPassword { return "Enter the password for \(draft.username)." }
        return nil
    }

    func load(client: ArcBoxClient?) async {
        guard let client else {
            loadState = .waiting
            return
        }
        if loadState != .loaded { loadState = .loading }
        do {
            let settings = try await client.egressProxy.getProxy(
                .init(), options: ArcBoxClient.defaultCallOptions)
            apply(EgressProxySettings(proto: settings))
            loadState = .loaded
        } catch {
            Log.daemon.error("Failed to load proxy settings: \(error.localizedDescription, privacy: .private)")
            loadState = .failed(ArcBoxClient.userMessage(for: error))
        }
    }

    func revert() {
        apply(saved)
        saveError = nil
    }

    func clearTestResults() { if !isTesting { testSteps = []; testError = nil } }

    func save(client: ArcBoxClient?) async {
        guard let client, !isSaving, validationError == nil else { return }
        isSaving = true
        saveError = nil
        var settings = draft
        settings.exclude = Self.lines(excludeText)
        let change: EgressProxyPasswordChange = password.isEmpty ? .keep : .set(password)
        do {
            let result = try await client.egressProxy.setProxy(
                settings.setRequest(password: change), options: ArcBoxClient.defaultCallOptions)
            apply(EgressProxySettings(proto: result))
        } catch {
            Log.daemon.error("Failed to save proxy settings: \(error.localizedDescription, privacy: .private)")
            ErrorReporting.capture(error, domain: .daemon, operation: "setProxy")
            saveError = ArcBoxClient.userMessage(for: error)
        }
        isSaving = false
    }

    /// Removes the stored password of the saved proxy.
    func clearPassword(client: ArcBoxClient?) async {
        guard let client, !isSaving else { return }
        isSaving = true
        saveError = nil
        var settings = saved
        if settings.mode == .custom, !settings.username.isEmpty {
            // A user without a password cannot be applied; drop the user too.
            settings.username = ""
        }
        do {
            let result = try await client.egressProxy.setProxy(
                settings.setRequest(password: .clear), options: ArcBoxClient.defaultCallOptions)
            apply(EgressProxySettings(proto: result))
        } catch {
            saveError = ArcBoxClient.userMessage(for: error)
        }
        isSaving = false
    }

    /// Tests the draft (unsaved) settings, or the saved ones when nothing
    /// was edited.
    func test(client: ArcBoxClient?, target: String) async {
        guard let client, !isTesting else { return }
        isTesting = true
        testSteps = []
        testError = nil
        var request = Arcbox_V1_TestEgressProxyRequest()
        request.target = target.trimmingCharacters(in: .whitespaces)
        if isDirty {
            var settings = draft
            settings.exclude = Self.lines(excludeText)
            request.settings = settings.proto
            request.profileID = settings.profileID
            if !password.isEmpty { request.password = password }
        }
        do {
            try await client.egressProxy.testProxy(
                request, options: ArcBoxClient.proxyTestCallOptions
            ) { response in
                for try await step in response.messages {
                    let parsed = EgressProxyTestStep(proto: step)
                    await MainActor.run { self.testSteps.append(parsed) }
                }
            }
        } catch {
            testError = ArcBoxClient.userMessage(for: error)
        }
        isTesting = false
    }

    private func apply(_ settings: EgressProxySettings) {
        saved = settings
        draft = settings
        excludeText = settings.exclude.joined(separator: "\n")
        password = ""
    }

    private static func lines(_ text: String) -> [String] {
        text.split(whereSeparator: { $0 == "\n" || $0 == "," })
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }
}

import ArcBoxClient
import Foundation
import GRPCCore

@MainActor
@Observable
final class ProxyLibraryModel {
    enum Phase { case waiting, loading, loaded, failed(String) }
    private(set) var phase: Phase = .waiting
    private(set) var profiles: [SavedProxyProfile] = []
    private(set) var testSteps: [String: [EgressProxyTestStep]] = [:]
    private(set) var testing: Set<String> = []
    var error: String?

    func load(client: ArcBoxClient?) async {
        guard let client else { phase = .waiting; return }
        if profiles.isEmpty { phase = .loading }
        do {
            let result = try await client.proxyLibrary.listProfiles(.init(), options: ArcBoxClient.defaultCallOptions)
            profiles = result.profiles.map(SavedProxyProfile.init(proto:))
            testSteps = [:]
            phase = .loaded
        } catch { phase = .failed(ArcBoxClient.userMessage(for: error)) }
    }

    func remove(_ profile: SavedProxyProfile, client: ArcBoxClient?) async {
        guard let client else { return }
        var request = Arcbox_V1_ProxyProfileRequest()
        request.id = profile.id
        do { _ = try await client.proxyLibrary.removeProfile(request, options: ArcBoxClient.defaultCallOptions) } catch
        { self.error = ArcBoxClient.userMessage(for: error) }
        await load(client: client)
    }

    func test(_ profile: SavedProxyProfile, client: ArcBoxClient?) async {
        guard let client, !testing.contains(profile.id) else { return }
        testing.insert(profile.id)
        testSteps[profile.id] = []
        var request = Arcbox_V1_TestEgressProxyRequest()
        request.settings = profile.settings.proto
        request.profileID = profile.id
        do {
            try await client.egressProxy.testProxy(request, options: ArcBoxClient.proxyTestCallOptions) { response in
                for try await event in response.messages {
                    let step = EgressProxyTestStep(proto: event)
                    await MainActor.run { self.testSteps[profile.id, default: []].append(step) }
                }
            }
        } catch { self.error = ArcBoxClient.userMessage(for: error) }
        testing.remove(profile.id)
    }
}

@MainActor
@Observable
final class ProxyProfileEditorModel {
    let id: String
    let original: EgressProxySettings?
    private var previousSuggestion: String? = nil
    var name: String
    var urlInput = ""
    var draft: EgressProxySettings
    var password = ""
    var excludeText = ""
    var error: String?
    private(set) var isSaving = false
    private(set) var isTesting = false
    private(set) var steps: [EgressProxyTestStep] = []

    init(profile: SavedProxyProfile?) {
        id = profile?.id ?? ""
        name = profile?.name ?? ""
        original = profile?.settings
        draft = profile?.settings ?? EgressProxySettings()
        draft.mode = .custom
        excludeText = draft.exclude.joined(separator: "\n")
        previousSuggestion = profile?.settings.endpointLabel
    }

    var hasStoredPassword: Bool {
        original.map { draft.sameAccount(as: $0) && $0.hasPassword } ?? false
    }

    var validationError: String? {
        var fields = draft
        fields.profileID = ""
        if let error = fields.validationError { return error }
        if password.utf8.count > 255 { return "The password is longer than 255 UTF-8 bytes." }
        if !draft.username.isEmpty && password.isEmpty && !hasStoredPassword { return "Enter the proxy password." }
        return nil
    }

    @discardableResult
    func importURL() -> Bool {
        do {
            let imported = try SocksURLImport.parse(urlInput)
            let previous = draft
            draft = imported.settings
            draft.udp = previous.udp
            draft.icmp = previous.icmp
            draft.profileID = id
            password = imported.password ?? ""
            name = SocksURLImport.suggestedName(for: draft, currentName: name, previousSuggestion: previousSuggestion)
            previousSuggestion = draft.endpointLabel
            urlInput = ""
            steps = []
            error = nil
            return true
        } catch { self.error = error.localizedDescription; return false }
    }

    private func prepare() -> Bool {
        if !urlInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !importURL() { return false }
        if let validationError { error = validationError; return false }
        draft.exclude = excludeText.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        return true
    }

    func clearTestResults() { if !isTesting { steps = []; error = nil } }

    func save(client: ArcBoxClient?) async -> Bool {
        guard let client, !isSaving, prepare() else { return false }
        guard !name.trimmingCharacters(in: .whitespaces).isEmpty else { error = "Enter a proxy name."; return false }
        isSaving = true
        defer { isSaving = false }
        var request = Arcbox_V1_SaveProxyProfileRequest()
        request.id = id
        request.name = name
        request.settings = draft.proto
        if !password.isEmpty { request.password = password }
        do {
            _ = try await client.proxyLibrary.saveProfile(request, options: ArcBoxClient.defaultCallOptions)
            password = ""
            urlInput = ""
            return true
        } catch { self.error = ArcBoxClient.userMessage(for: error); return false }
    }

    func test(client: ArcBoxClient?) async {
        guard let client, !isTesting, prepare() else { return }
        isTesting = true
        steps = []
        error = nil
        defer { isTesting = false }
        var request = Arcbox_V1_TestEgressProxyRequest()
        request.settings = draft.proto
        request.profileID = id
        if !password.isEmpty { request.password = password }
        do {
            try await client.egressProxy.testProxy(request, options: ArcBoxClient.proxyTestCallOptions) { response in
                for try await event in response.messages {
                    let step = EgressProxyTestStep(proto: event)
                    await MainActor.run { self.steps.append(step) }
                }
            }
        } catch { self.error = ArcBoxClient.userMessage(for: error) }
    }
}

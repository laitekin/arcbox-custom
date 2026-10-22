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
    /// The default proxy as the daemon reported it; nil until loaded.
    private(set) var defaultSettings: EgressProxySettings?
    private(set) var isSavingDefault = false
    /// Each machine's saved choice: empty follows the default, `direct`, or
    /// a saved proxy ID.
    private(set) var machineChoices: [String: String] = [:]
    /// What each machine's new connections use right now.
    private(set) var machineEffective: [String: String] = [:]
    private(set) var savingMachines: Set<String> = []

    /// Drops a proxy's test results, which no longer describe it once edited.
    func forgetTestResults(_ id: String) { testSteps[id] = nil }

    func profile(_ id: String) -> SavedProxyProfile? { profiles.first { $0.id == id } }

    /// The default as a picker choice: a saved proxy ID, `system`, `direct`,
    /// or `custom` for a one-off proxy edited in Settings.
    var defaultChoice: String? {
        defaultSettings.map { ProxyChoice.defaultChoice(of: $0) }
    }

    func load(client: ArcBoxClient?) async {
        guard let client else { phase = .waiting; return }
        if profiles.isEmpty { phase = .loading }
        do {
            let result = try await client.proxyLibrary.listProfiles(.init(), options: ArcBoxClient.defaultCallOptions)
            profiles = result.profiles.map(SavedProxyProfile.init(proto:))
            let ids = Set(profiles.map(\.id))
            testSteps = testSteps.filter { ids.contains($0.key) }
            phase = .loaded
        } catch { phase = .failed(ArcBoxClient.userMessage(for: error)) }
    }

    func loadDefault(client: ArcBoxClient?) async {
        guard let client else { return }
        do {
            let reply = try await client.egressProxy.getProxy(.init(), options: ArcBoxClient.defaultCallOptions)
            defaultSettings = EgressProxySettings(proto: reply)
        } catch { self.error = ArcBoxClient.userMessage(for: error) }
    }

    /// Makes `choice` the default proxy; it reaches new connections at once.
    func setDefault(_ choice: String, client: ArcBoxClient?) async {
        guard let client, let current = defaultSettings, !isSavingDefault,
            let next = ProxyChoice.settings(forDefault: choice, current: current)
        else { return }
        isSavingDefault = true
        defer { isSavingDefault = false }
        do {
            let reply = try await client.egressProxy.setProxy(
                next.setRequest(password: .keep), options: ArcBoxClient.defaultCallOptions)
            defaultSettings = EgressProxySettings(proto: reply)
            error = nil
        } catch { self.error = ArcBoxClient.userMessage(for: error) }
        await load(client: client)
    }

    func loadMachineChoices(_ machines: [String], client: ArcBoxClient?) async {
        guard let client else { return }
        for machine in machines {
            var request = Arcbox_V1_MachineProxyRequest()
            request.machine = machine
            if let reply = try? await client.proxyLibrary.getMachineProxy(
                request, options: ArcBoxClient.defaultCallOptions)
            {
                apply(reply, machine: machine)
            }
        }
    }

    /// Assigns a machine's proxy; running machines use it for new
    /// connections without a restart. Returns whether it was saved.
    @discardableResult
    func setMachine(_ machine: String, choice: String, client: ArcBoxClient?) async -> Bool {
        guard let client, !savingMachines.contains(machine) else { return false }
        savingMachines.insert(machine)
        defer { savingMachines.remove(machine) }
        var request = Arcbox_V1_SetMachineProxyRequest()
        request.machine = machine
        request.choice = choice
        do {
            let reply = try await client.proxyLibrary.setMachineProxy(
                request, options: ArcBoxClient.defaultCallOptions)
            apply(reply, machine: machine)
            error = nil
            await load(client: client)
            return true
        } catch {
            self.error = ArcBoxClient.userMessage(for: error)
            return false
        }
    }

    private func apply(_ reply: Arcbox_V1_MachineProxyAssignment, machine: String) {
        machineChoices[machine] = reply.choice
        let effective = EgressProxySettings(proto: reply.effective).effective
        machineEffective[machine] = effective.isEmpty ? "Direct" : effective.joined(separator: ", ")
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
        error = nil
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
    /// The endpoint the last pasted URL filled in, to confirm the import.
    private(set) var lastImported: String?
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

    /// Why Save and Test are unavailable; shown beside the fields.
    var fieldProblem: String? { validationError }

    /// The user has started describing a proxy, so problems are worth showing.
    var hasInput: Bool { !draft.host.isEmpty || !draft.port.isEmpty || !draft.username.isEmpty }

    /// Imports the URL field if it holds a complete URL; otherwise leaves it
    /// alone without complaint.
    func importIfComplete() {
        guard (try? SocksURLImport.parse(urlInput)) != nil else { return }
        importURL()
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
            lastImported = draft.endpointLabel
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
        if name.trimmingCharacters(in: .whitespaces).isEmpty { name = draft.endpointLabel }
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

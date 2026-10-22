import ArcBoxClient
import SwiftUI

/// The machine's outbound proxy. Picking applies at once — running machines
/// switch for new connections without a restart.
struct MachineProxySection: View {
    let machine: String
    @Environment(\.arcboxClient) private var client
    @Environment(AppViewModel.self) private var appVM
    @State private var library = ProxyLibraryModel()
    private enum Phase { case loading, loaded, failed }
    @State private var phase: Phase = .loading
    @State private var steps: [EgressProxyTestStep] = []
    @State private var isTesting = false
    @State private var testExpanded = false
    @State private var testError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Outbound proxy").font(.system(size: 13, weight: .semibold)).accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: 10) {
                switch phase {
                case .loading:
                    ProgressView().controlSize(.small)
                case .failed:
                    ProxyErrorLabel(message: library.error ?? "The proxy assignment could not be loaded.")
                    Button("Try Again") { Task { await load() } }
                case .loaded:
                    loaded
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .infoSectionStyle()
        }
        .task(id: machine) { await load() }
    }

    @ViewBuilder
    private var loaded: some View {
        let choice = library.machineChoices[machine] ?? ""
        let isSaving = library.savingMachines.contains(machine)
        HStack(spacing: 10) {
            MachineProxyPicker(choice: choice, profiles: library.profiles) { next in
                Task {
                    clearTest()
                    await library.setMachine(machine, choice: next, client: client)
                }
            }
            .disabled(isSaving || isTesting)
            if isSaving {
                ProgressView().controlSize(.small)
            }
            Spacer()
            Button(isTesting ? "Testing…" : "Test") { Task { await test(choice) } }
                .disabled(isSaving || isTesting || choice == ProxyChoice.direct)
                .help(choice == ProxyChoice.direct ? "Direct connections have no proxy to test" : "Test this proxy")
            Button("Manage Proxies…") { appVM.navigate(to: .proxies) }
        }
        if let effective = library.machineEffective[machine] {
            Text("In use: \(effective)").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        }
        if isTesting || !steps.isEmpty {
            ProxyTestDisclosure(
                summary: ProxyTestSummary(steps: steps, isRunning: isTesting),
                steps: steps,
                isExpanded: $testExpanded
            )
        }
        if let error = testError ?? library.error {
            ProxyErrorLabel(message: error)
        }
    }

    private func load() async {
        guard let client else {
            library.error = "ArcBox daemon is unavailable."
            phase = .failed
            return
        }
        phase = .loading
        clearTest()
        library.error = nil
        await library.load(client: client)
        await library.loadMachineChoices([machine], client: client)
        phase = library.machineChoices[machine] == nil ? .failed : .loaded
    }

    private func clearTest() {
        steps = []
        testError = nil
        testExpanded = false
    }

    /// Tests what the machine is set to: its saved proxy, or the default.
    private func test(_ choice: String) async {
        guard let client, !isTesting else { return }
        isTesting = true
        clearTest()
        defer { isTesting = false }
        var request = Arcbox_V1_TestEgressProxyRequest()
        if let profile = library.profile(choice) {
            request.settings = profile.settings.proto
            request.profileID = profile.id
        }
        do {
            steps = try await client.egressProxy.testProxy(request, options: ArcBoxClient.proxyTestCallOptions) {
                response in
                var received: [EgressProxyTestStep] = []
                for try await event in response.messages {
                    received.append(EgressProxyTestStep(proto: event))
                }
                return received
            }
        } catch {
            testError = ArcBoxClient.userMessage(for: error)
        }
        testExpanded = ProxyTestSummary(steps: steps, isRunning: false).isFailure
    }
}

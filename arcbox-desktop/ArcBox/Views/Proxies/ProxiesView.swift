import AppKit
import ArcBoxClient
import SwiftUI

/// Main window › Proxies: the default proxy, the saved proxies, and which
/// proxy each machine uses, on one page. Choices apply as soon as they are
/// picked; the daemon hands them to new connections without restarts.
struct ProxiesView: View {
    @Environment(MachinesViewModel.self) private var machinesVM
    @Environment(DaemonManager.self) private var daemonManager
    @Environment(\.arcboxClient) private var arcboxClient

    @State private var library = ProxyLibraryModel()
    @State private var editing: SavedProxyProfile?
    @State private var showingEditor = false
    @State private var deleting: SavedProxyProfile?
    @State private var expandedTests: Set<String> = []

    private var client: ArcBoxClient? {
        daemonManager.state.isRunning ? arcboxClient : nil
    }

    var body: some View {
        Group {
            if client == nil {
                DaemonLoadingView(state: daemonManager.state)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        defaultSection
                        savedSection
                        machinesSection
                    }
                    .padding(20)
                    .frame(maxWidth: 760, alignment: .leading)
                    .frame(maxWidth: .infinity)
                }
            }
        }
        .navigationTitle("Proxies")
        .navigationSubtitle(subtitle)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button(
                    action: { Task { await reload() } },
                    label: { Image(systemName: "arrow.clockwise") }
                )
                .help("Refresh")
                .accessibilityLabel("Refresh")
                Button(
                    action: { startImport() },
                    label: { Image(systemName: "plus") }
                )
                .help("Import a SOCKS URL")
                .accessibilityLabel("Import a SOCKS URL")
            }
        }
        .disabled(client == nil)
        .task(id: client != nil) { await reload() }
        .onReceive(NotificationCenter.default.publisher(for: .machineChanged)) { _ in
            Task {
                await machinesVM.loadMachines(client: client)
                await library.loadMachineChoices(machinesVM.machines.map(\.id), client: client)
            }
        }
        .sheet(isPresented: $showingEditor) {
            ProxyProfileEditorSheet(profile: editing) {
                if let id = editing?.id { library.forgetTestResults(id) }
                await reload()
            }
        }
        .confirmationDialog(
            "Delete saved proxy?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            presenting: deleting
        ) { profile in
            Button("Delete \(profile.name)", role: .destructive) {
                Task {
                    await library.remove(profile, client: client)
                    deleting = nil
                }
            }
        } message: { profile in
            Text("\(profile.settings.endpointLabel) and its stored password are removed.")
        }
    }

    private var subtitle: String {
        guard let settings = library.defaultSettings else { return "" }
        return "Default: " + defaultLabel(settings)
    }

    // MARK: - Default

    private var defaultSection: some View {
        ProxyPageSection(
            title: "Default proxy",
            caption: "Containers, image pulls and builds, and machines set to follow the default."
        ) {
            if let choice = library.defaultChoice, let settings = library.defaultSettings {
                HStack(spacing: 10) {
                    Picker("Default proxy", selection: defaultBinding(current: choice)) {
                        Text("Follow macOS").tag(ProxyChoice.system)
                        Text("Direct").tag(ProxyChoice.direct)
                        if choice == ProxyChoice.custom {
                            Text("Custom proxy (edit in Settings)").tag(ProxyChoice.custom)
                        }
                        if !library.profiles.isEmpty {
                            Divider()
                            ForEach(library.profiles) { Text($0.menuLabel).tag($0.id) }
                        }
                        if !choice.isEmpty,
                            ![ProxyChoice.system, ProxyChoice.direct, ProxyChoice.custom].contains(choice),
                            library.profile(choice) == nil
                        {
                            Text("Saved proxy unavailable").tag(choice)
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                    .disabled(library.isSavingDefault)
                    if library.isSavingDefault {
                        ProgressView().controlSize(.small)
                    }
                    Spacer()
                    Button("Advanced…") {
                        (NSApp.delegate as? AppDelegate)?.coordinator?.showSettings(tab: .network)
                    }
                    .help("Bypass list, UDP and ICMP policy, one-off custom proxy")
                }
                ProxyEffectiveLabel(effective: settings.effective)
            } else {
                ProgressView().controlSize(.small)
            }
        }
    }

    private func defaultBinding(current: String) -> Binding<String> {
        Binding(
            get: { current },
            set: { choice in
                guard choice != current else { return }
                Task { await library.setDefault(choice, client: client) }
            }
        )
    }

    private func defaultLabel(_ settings: EgressProxySettings) -> String {
        if let profile = library.profile(settings.profileID) { return profile.name }
        return settings.mode.label
    }

    // MARK: - Saved proxies

    private var savedSection: some View {
        ProxyPageSection(
            title: "Saved proxies",
            caption: "Paste a SOCKS URL to save a proxy, test it, then use it as the default or for single machines."
        ) {
            switch library.phase {
            case .waiting, .loading:
                ProgressView().controlSize(.small)
            case .failed(let message):
                ProxyErrorLabel(message: message)
                Button("Try Again") { Task { await reload() } }
            case .loaded:
                if library.profiles.isEmpty {
                    HStack {
                        Text("No saved proxies yet.").foregroundStyle(.secondary)
                        Spacer()
                        Button("Import SOCKS URL…") { startImport() }
                            .buttonStyle(.borderedProminent)
                    }
                } else {
                    ForEach(Array(library.profiles.enumerated()), id: \.element.id) { index, profile in
                        if index > 0 { Divider() }
                        profileRow(profile)
                    }
                }
            }
            if let error = library.error {
                ProxyErrorLabel(message: error)
            }
        }
    }

    private func profileRow(_ profile: SavedProxyProfile) -> some View {
        let isTesting = library.testing.contains(profile.id)
        let steps = library.testSteps[profile.id]
        let summary = ProxyTestSummary(steps: steps, isRunning: isTesting)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(profile.name).fontWeight(.medium)
                        if profile.isDefault {
                            ProxyTag(text: "Default")
                        }
                    }
                    Text(profile.settings.url)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                    Text(usage(profile)).font(.caption).foregroundStyle(.secondary)
                }
                Spacer(minLength: 12)
                Button(isTesting ? "Testing…" : "Test") {
                    expandedTests.remove(profile.id)
                    Task {
                        await library.test(profile, client: client)
                        if ProxyTestSummary(steps: library.testSteps[profile.id], isRunning: false).isFailure {
                            expandedTests.insert(profile.id)
                        }
                    }
                }
                .disabled(isTesting)
                if !profile.isDefault {
                    Button("Use as Default") {
                        Task { await library.setDefault(profile.id, client: client) }
                    }
                    .disabled(library.isSavingDefault)
                }
                Menu {
                    Button("Edit…") {
                        editing = profile
                        showingEditor = true
                    }
                    Button("Copy URL") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(profile.settings.url, forType: .string)
                    }
                    Divider()
                    Button("Delete…", role: .destructive) { deleting = profile }
                        .disabled(profile.isInUse)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .accessibilityLabel("More actions for \(profile.name)")
                .help(profile.isInUse ? "Stop using this proxy before deleting it." : "More actions")
                .disabled(isTesting)
            }
            if summary != .untested {
                ProxyTestDisclosure(
                    summary: summary,
                    steps: steps ?? [],
                    isExpanded: Binding(
                        get: { expandedTests.contains(profile.id) },
                        set: { if $0 { expandedTests.insert(profile.id) } else { expandedTests.remove(profile.id) } }
                    )
                )
            }
        }
        .padding(.vertical, 4)
    }

    private func usage(_ profile: SavedProxyProfile) -> String {
        // The daemon keys assignments by the name the machine was given.
        let names = profile.machines.map { key in
            machinesVM.machines.first { $0.id == key || $0.name == key }?.name ?? key
        }
        var uses = names
        if profile.isDefault { uses.insert("the default", at: 0) }
        return uses.isEmpty ? "Not in use" : "Used by " + uses.joined(separator: ", ")
    }

    // MARK: - Machines

    private var machinesSection: some View {
        ProxyPageSection(
            title: "Machines",
            caption:
                "Each Linux machine follows the default unless you choose otherwise. Running machines switch without a restart."
        ) {
            if machinesVM.machines.isEmpty {
                Text(machinesVM.loadState == .loaded ? "No machines." : "Loading machines…")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(sortedMachines.enumerated()), id: \.element.id) { index, machine in
                    if index > 0 { Divider() }
                    machineRow(machine)
                }
            }
        }
    }

    private var sortedMachines: [MachineViewModel] {
        machinesVM.machines.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private func machineRow(_ machine: MachineViewModel) -> some View {
        let choice = library.machineChoices[machine.id]
        return HStack(spacing: 10) {
            Image(systemName: "desktopcomputer")
                .foregroundStyle(machine.isRunning ? .green : .secondary)
                .accessibilityLabel(machine.isRunning ? "Running" : "Stopped")
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name)
                if let effective = library.machineEffective[machine.id] {
                    Text("In use: \(effective)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
            }
            Spacer()
            if library.savingMachines.contains(machine.id) {
                ProgressView().controlSize(.small)
            }
            if let choice {
                MachineProxyPicker(
                    choice: choice,
                    profiles: library.profiles,
                    onChange: { next in Task { await library.setMachine(machine.id, choice: next, client: client) } }
                )
                .disabled(library.savingMachines.contains(machine.id))
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .padding(.vertical, 2)
    }

    // MARK: - Actions

    private func startImport() {
        editing = nil
        showingEditor = true
    }

    private func reload() async {
        guard let client else { return }
        await library.loadDefault(client: client)
        await library.load(client: client)
        await machinesVM.loadMachines(client: client)
        await library.loadMachineChoices(machinesVM.machines.map(\.id), client: client)
    }
}

/// A machine's proxy menu: follow the default, direct, or a saved proxy.
/// Picking applies at once.
struct MachineProxyPicker: View {
    let choice: String
    let profiles: [SavedProxyProfile]
    let onChange: (String) -> Void

    var body: some View {
        Picker(
            "Proxy",
            selection: Binding(
                get: { choice },
                set: { if $0 != choice { onChange($0) } }
            )
        ) {
            Text("Follow default").tag("")
            Text("Direct").tag(ProxyChoice.direct)
            if !profiles.isEmpty {
                Divider()
                ForEach(profiles) { Text($0.menuLabel).tag($0.id) }
            }
            if !choice.isEmpty, choice != ProxyChoice.direct, !profiles.contains(where: { $0.id == choice }) {
                Text("Saved proxy unavailable").tag(choice)
            }
        }
        .labelsHidden()
        .fixedSize()
    }
}

/// A titled card, matching the machine Info tab's sections.
struct ProxyPageSection<Content: View>: View {
    let title: String
    let caption: String?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .accessibilityAddTraits(.isHeader)
                if let caption {
                    Text(caption).font(.caption).foregroundStyle(.secondary)
                }
            }
            VStack(alignment: .leading, spacing: 10) {
                content
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .infoSectionStyle()
        }
    }
}

/// "In use: …" for a proxy choice's effective route.
struct ProxyEffectiveLabel: View {
    let effective: [String]

    var body: some View {
        Text("In use: \(effective.isEmpty ? "Direct" : effective.joined(separator: ", "))")
            .font(.caption)
            .foregroundStyle(.secondary)
            .textSelection(.enabled)
    }
}

struct ProxyErrorLabel: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle")
            .font(.caption)
            .foregroundStyle(.red)
            .textSelection(.enabled)
    }
}

struct ProxyTag: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Color.accentColor.opacity(0.15), in: Capsule())
            .foregroundStyle(Color.accentColor)
    }
}

/// A test's one-line result that expands into its steps.
struct ProxyTestDisclosure: View {
    let summary: ProxyTestSummary
    let steps: [EgressProxyTestStep]
    @Binding var isExpanded: Bool

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(steps) { ProxyTestStepRow(step: $0) }
            }
            .padding(.top, 4)
        } label: {
            ProxyTestSummaryLabel(summary: summary)
        }
        .disabled(steps.isEmpty)
    }
}

struct ProxyTestSummaryLabel: View {
    let summary: ProxyTestSummary

    var body: some View {
        Label(summary.label, systemImage: summary.symbol)
            .font(.caption)
            .foregroundStyle(color)
            .lineLimit(2)
    }

    private var color: Color {
        switch summary {
        case .untested, .running: .secondary
        case .passed(_, let udpFailed): udpFailed ? .orange : .green
        case .failed: .red
        }
    }
}

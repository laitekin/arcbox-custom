import ArcBoxClient
import SwiftUI

/// Settings > Network: where containers' and machines' outbound traffic
/// goes, including an authenticated SOCKS5/HTTP proxy, and a step-by-step
/// test of it.
struct NetworkSettingsView: View {
    @Environment(\.arcboxClient) private var arcboxClient
    @Environment(DaemonManager.self) private var daemonManager

    @State private var model = NetworkSettingsModel()
    @State private var testTarget = ""

    private var client: ArcBoxClient? {
        daemonManager.state.isRunning ? arcboxClient : nil
    }

    var body: some View {
        Form {
            switch model.loadState {
            case .waiting:
                Section {
                    Text("Network settings are available while ArcBox is running.")
                        .foregroundStyle(.secondary)
                }
            case .loading:
                Section {
                    ProgressView().controlSize(.small)
                }
            case .failed(let message):
                Section {
                    Label(message, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                    Button("Try Again") { Task { await model.load(client: client) } }
                }
            case .loaded:
                proxySection
                trafficSection
                testSection
                saveSection
            }
        }
        .formStyle(.grouped)
        .scrollContentBackground(.hidden)
        .task(id: daemonManager.state.isRunning && arcboxClient != nil) {
            await model.load(client: client)
        }
    }

    // MARK: Sections

    private var proxySection: some View {
        @Bindable var model = model
        return Section {
            Picker("Proxy", selection: $model.draft.mode) {
                ForEach(EgressProxyMode.allCases) { mode in
                    Text(mode.label).tag(mode)
                }
            }

            if model.draft.mode == .custom {
                Picker("Protocol", selection: $model.draft.proxyProtocol) {
                    ForEach(EgressProxyProtocol.allCases) { proto in
                        Text(proto.label).tag(proto)
                    }
                }
                TextField("Server", text: $model.draft.host, prompt: Text("proxy.example.com"))
                if model.draft.proxyProtocol == .socks5 {
                    Toggle("Resolve DNS through the proxy", isOn: $model.draft.remoteDNS)
                        .toggleStyle(.checkbox)
                    Text(
                        model.draft.remoteDNS
                            ? "External names use 1.1.1.1 or 8.8.8.8 through the SOCKS5 TCP tunnel. ArcBox names stay local; bypassed domains use this Mac's DNS."
                            : "DNS uses this Mac's resolver. Turn on proxy DNS to resolve external names through SOCKS5."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                TextField("Port", text: $model.draft.port, prompt: Text("1080"))
                TextField("User name", text: $model.draft.username, prompt: Text("Optional"))
                if !model.draft.username.isEmpty {
                    SecureField(
                        "Password", text: $model.password,
                        prompt: Text(passwordPrompt))
                    if model.saved.hasPassword {
                        Button("Remove Stored Password", role: .destructive) {
                            Task { await model.clearPassword(client: client) }
                        }
                        .disabled(model.isSaving)
                    }
                }
                if model.draft.proxyProtocol == .socks5, !model.draft.username.isEmpty {
                    Text(
                        "SOCKS5 sends the user name and password unencrypted. Use a proxy on this Mac or a trusted network."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }

            LabeledContent("In use") {
                Text(model.saved.effective.isEmpty ? "Direct" : model.saved.effective.joined(separator: ", "))
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        } header: {
            VStack(alignment: .leading, spacing: 2) {
                Text("Outbound proxy")
                Text(
                    "Applies to containers, image pulls and builds, and Linux machines. Changes reach new connections at once."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            }
        }
    }

    private var passwordPrompt: String {
        if model.draft.sameAccount(as: model.saved) && model.saved.hasPassword {
            "Stored in \(model.saved.credentialStore == "keychain" ? "the keychain" : "ArcBox's credential file")"
        } else {
            "Required"
        }
    }

    private var trafficSection: some View {
        @Bindable var model = model
        return Section("Traffic") {
            VStack(alignment: .leading, spacing: 4) {
                Text("Bypass the proxy for")
                TextEditor(text: $model.excludeText)
                    .font(.body.monospaced())
                    .frame(minHeight: 54)
                Text("One entry per line: example.com, .internal, 10.0.0.0/8, or * for all traffic.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Picker("UDP (QUIC, custom DNS)", selection: $model.draft.udp) {
                ForEach(EgressUdpPolicy.allCases) { policy in
                    Text(policy.label).tag(policy)
                }
            }
            Picker("ICMP (ping)", selection: $model.draft.icmp) {
                ForEach(EgressIcmpPolicy.allCases) { policy in
                    Text(policy.label).tag(policy)
                }
            }
        }
    }

    private var testSection: some View {
        Section {
            HStack {
                TextField("Target", text: $testTarget, prompt: Text("www.gstatic.com:443"))
                Button(model.isDirty ? "Test Unsaved Settings" : "Test Connection") {
                    Task { await model.test(client: client, target: testTarget) }
                }
                .disabled(model.isTesting || model.draft.validationError != nil)
            }
            if model.isTesting && model.testSteps.isEmpty {
                ProgressView().controlSize(.small)
            }
            ForEach(model.testSteps) { step in
                ProxyTestStepRow(step: step)
            }
            if let error = model.testError {
                Text(error).font(.caption).foregroundStyle(.red)
            }
        } header: {
            Text("Test")
        }
    }

    private var saveSection: some View {
        Section {
            if let error = model.validationError, model.isDirty {
                Label(error, systemImage: "exclamationmark.circle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            if let error = model.saveError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button("Revert") { model.revert() }
                    .disabled(!model.isDirty || model.isSaving)
                Button("Apply") { Task { await model.save(client: client) } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!model.isDirty || model.isSaving || model.validationError != nil)
            }
        }
        .listRowBackground(Color.clear)
    }
}

private struct ProxyTestStepRow: View {
    let step: EgressProxyTestStep

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Image(systemName: symbol)
                .foregroundStyle(color)
            VStack(alignment: .leading, spacing: 2) {
                Text(step.kind.title)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            Spacer()
            if step.ok && step.latencyMilliseconds > 0 {
                Text("\(step.latencyMilliseconds) ms")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var symbol: String {
        if step.skipped { return "minus.circle" }
        return step.ok ? "checkmark.circle.fill" : "xmark.circle.fill"
    }

    private var color: Color {
        if step.skipped { return .secondary }
        return step.ok ? .green : .red
    }

    private var detail: String {
        if let failure = step.failure {
            return step.message.isEmpty ? failure : "\(failure): \(step.message)"
        }
        return step.message
    }
}

import AppKit
import ArcBoxClient
import SwiftUI

/// Settings › Network's pointer to the saved proxies, which are managed on
/// the main window's Proxies page.
struct ProxyLibrarySection: View {
    let model: ProxyLibraryModel

    var body: some View {
        Section {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(summary)
                    Text("Import, test, and assign saved SOCKS proxies on the Proxies page.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Open Proxies") {
                    (NSApp.delegate as? AppDelegate)?.coordinator?
                        .handleDeepLink(DeepLink.section(.proxies, id: nil).url)
                }
            }
        }
    }

    private var summary: String {
        switch model.profiles.count {
        case 0: "No saved proxies"
        case 1: "1 saved proxy"
        case let count: "\(count) saved proxies"
        }
    }
}

struct ProxyProfileEditorSheet: View {
    @Environment(\.arcboxClient) private var client
    @Environment(\.dismiss) private var dismiss
    @State private var model: ProxyProfileEditorModel
    @State private var showAdvanced = false
    let onSaved: () async -> Void

    init(profile: SavedProxyProfile?, onSaved: @escaping () async -> Void) {
        _model = State(initialValue: ProxyProfileEditorModel(profile: profile))
        self.onSaved = onSaved
    }

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            HStack {
                Text(model.id.isEmpty ? "Import SOCKS Proxy" : "Edit SOCKS Proxy").font(.headline)
                Spacer()
            }
            .padding()
            Form {
                Section {
                    SecureField(
                        "SOCKS URL", text: $model.urlInput, prompt: Text("Paste socks5h://user:password@host:1080")
                    )
                    .onSubmit { model.importURL() }
                    Text(
                        model.lastImported.map { "Filled in from \($0). Paste another URL to replace it." }
                            ?? "Paste a URL to fill in the fields below, or type them yourself."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
                Section("Proxy") {
                    TextField(
                        "Name", text: $model.name,
                        prompt: Text(model.draft.host.isEmpty ? "Office proxy" : model.draft.endpointLabel))
                    TextField("Server", text: $model.draft.host, prompt: Text("proxy.example.com"))
                    TextField("Port", text: $model.draft.port, prompt: Text("1080"))
                    TextField("User name", text: $model.draft.username, prompt: Text("Optional"))
                    if !model.draft.username.isEmpty {
                        SecureField(
                            "Password", text: $model.password,
                            prompt: Text(model.hasStoredPassword ? "Stored — leave blank to keep" : "Required")
                        )
                    }
                    Toggle("Resolve DNS through the proxy", isOn: $model.draft.remoteDNS)
                    if let problem = model.fieldProblem, model.hasInput {
                        Label(problem, systemImage: "exclamationmark.circle")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }
                Section {
                    DisclosureGroup("Advanced", isExpanded: $showAdvanced) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Bypass the proxy for")
                            TextEditor(text: $model.excludeText).font(.body.monospaced()).frame(minHeight: 44)
                            Text("One domain, IP, or CIDR per line.").font(.caption).foregroundStyle(.secondary)
                        }
                        Picker("UDP", selection: $model.draft.udp) {
                            ForEach(EgressUdpPolicy.allCases) { Text($0.label).tag($0) }
                        }
                        Picker("ICMP", selection: $model.draft.icmp) {
                            ForEach(EgressIcmpPolicy.allCases) { Text($0.label).tag($0) }
                        }
                        Text(
                            "With proxy DNS on, names resolve through the tunnel (1.1.1.1 / 8.8.8.8); bypassed names use the Mac's DNS."
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                }
                if model.isTesting || !model.steps.isEmpty {
                    Section("Test") {
                        ProxyTestSummaryLabel(summary: ProxyTestSummary(steps: model.steps, isRunning: model.isTesting))
                        ForEach(model.steps) { ProxyTestStepRow(step: $0) }
                    }
                }
                if let error = model.error {
                    Section { ProxyErrorLabel(message: error) }
                }
            }
            .formStyle(.grouped)
            .disabled(model.isSaving || model.isTesting)
            Divider()
            HStack {
                Button(model.isTesting ? "Testing…" : "Test") {
                    Task { await model.test(client: client) }
                }
                .help("Try the connection without saving")
                .disabled(model.isTesting || model.isSaving || model.fieldProblem != nil)
                Spacer()
                Button("Cancel") {
                    model.password = ""
                    model.urlInput = ""
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                .disabled(model.isSaving)
                Button(model.isSaving ? "Saving…" : "Save") {
                    Task { if await model.save(client: client) { await onSaved(); dismiss() } }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(model.isSaving || model.isTesting || model.fieldProblem != nil)
            }
            .padding()
        }
        .frame(width: 520)
        .frame(minHeight: 460, idealHeight: 560)
        .interactiveDismissDisabled(model.isSaving || model.isTesting)
        .onChange(of: model.draft) { model.clearTestResults() }
        .onChange(of: model.password) { model.clearTestResults() }
        // A paste arrives as one multi-character change; typing does not, and
        // must not be cut off as soon as a prefix parses.
        .onChange(of: model.urlInput) { old, new in
            if new.count - old.count > 1 { model.importIfComplete() } else { model.clearTestResults() }
        }
        .onChange(of: model.excludeText) { model.clearTestResults() }
    }
}

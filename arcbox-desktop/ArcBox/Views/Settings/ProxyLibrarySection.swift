import ArcBoxClient
import SwiftUI

struct ProxyLibrarySection: View {
    @Bindable var model: ProxyLibraryModel
    let client: ArcBoxClient?
    let onSaved: () async -> Void
    @State private var editing: SavedProxyProfile?
    @State private var showingEditor = false
    @State private var deleting: SavedProxyProfile?

    var body: some View {
        Section("Saved SOCKS proxies") {
            HStack {
                Text("Import a URL, test it, then choose it for the default or individual machines.")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Import SOCKS URL…") {
                    editing = nil; showingEditor = true
                }
                Button("Refresh") { Task { await model.load(client: client) } }
            }
            switch model.phase {
            case .waiting, .loading: ProgressView().controlSize(.small)
            case .failed(let message):
                Text(message).foregroundStyle(.red)
                Button("Try Again") { Task { await model.load(client: client) } }
            case .loaded:
                if model.profiles.isEmpty {
                    Text("No saved proxies. Import your first SOCKS5 URL.").foregroundStyle(.secondary)
                }
                ForEach(model.profiles) { profile in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(profile.name).fontWeight(.medium)
                                Text(profile.settings.url).font(.caption.monospaced()).foregroundStyle(.secondary)
                                    .textSelection(.enabled)
                                Text(usage(profile)).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if model.testing.contains(profile.id) { ProgressView().controlSize(.small) }
                            Button("Test") { Task { await model.test(profile, client: client) } }.disabled(
                                model.testing.contains(profile.id))
                            Button("Edit…") {
                                editing = profile; showingEditor = true
                            }
                            Button("Delete", role: .destructive) { deleting = profile }
                                .disabled(profile.isInUse).help(
                                    profile.isInUse
                                        ? "Change the default and machine assignments before deleting."
                                        : "Delete saved proxy")
                        }
                        if let steps = model.testSteps[profile.id] {
                            ForEach(steps) { ProxyTestStepRow(step: $0) }
                        }
                    }.padding(.vertical, 4)
                }
            }
            if let error = model.error { Text(error).foregroundStyle(.red) }
        }
        .sheet(isPresented: $showingEditor) {
            ProxyProfileEditorSheet(profile: editing) {
                await model.load(client: client)
                await onSaved()
            }
        }
        .confirmationDialog(
            "Delete saved proxy?", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            presenting: deleting
        ) { profile in
            Button("Delete \(profile.name)", role: .destructive) {
                Task {
                    await model.remove(profile, client: client); deleting = nil
                }
            }
        }
    }

    private func usage(_ profile: SavedProxyProfile) -> String {
        var uses = profile.machines
        if profile.isDefault { uses.insert("Default", at: 0) }
        return uses.isEmpty ? "Not assigned" : "Used by: " + uses.joined(separator: ", ")
    }
}

struct ProxyProfileEditorSheet: View {
    @Environment(\.arcboxClient) private var client
    @Environment(\.dismiss) private var dismiss
    @State private var model: ProxyProfileEditorModel
    let onSaved: () async -> Void

    init(profile: SavedProxyProfile?, onSaved: @escaping () async -> Void) {
        _model = State(initialValue: ProxyProfileEditorModel(profile: profile))
        self.onSaved = onSaved
    }

    var body: some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            HStack {
                Text(model.id.isEmpty ? "Import SOCKS proxy" : "Edit SOCKS proxy").font(.headline); Spacer()
            }.padding()
            Form {
                Section("Import from URL") {
                    SecureField("SOCKS URL", text: $model.urlInput, prompt: Text("socks5h://user:password@host:1080"))
                    Button("Import URL") { model.importURL() }.disabled(model.urlInput.isEmpty)
                    Text("Credentials are decoded into the fields below. Testing does not save or activate this proxy.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Section("Proxy") {
                    TextField("Name", text: $model.name)
                    TextField("Server", text: $model.draft.host)
                    TextField("Port", text: $model.draft.port)
                    TextField("User name", text: $model.draft.username)
                    if !model.draft.username.isEmpty {
                        SecureField(
                            "Password", text: $model.password,
                            prompt: Text(model.hasStoredPassword ? "Stored password — leave blank to keep" : "Required")
                        )
                    }
                    Toggle("Resolve DNS through the proxy", isOn: $model.draft.remoteDNS)
                    Text(
                        "Proxy DNS uses 1.1.1.1 / 8.8.8.8 through the SOCKS5 TCP tunnel; bypassed names keep the Mac's DNS."
                    ).font(.caption).foregroundStyle(.secondary)
                    Text("Bypass the proxy for")
                    TextEditor(text: $model.excludeText).font(.body.monospaced()).frame(minHeight: 44)
                    Text("One domain, IP, or CIDR per line.").font(.caption).foregroundStyle(.secondary)
                    Picker("UDP", selection: $model.draft.udp) {
                        ForEach(EgressUdpPolicy.allCases) { Text($0.label).tag($0) }
                    }
                    Picker("ICMP", selection: $model.draft.icmp) {
                        ForEach(EgressIcmpPolicy.allCases) { Text($0.label).tag($0) }
                    }
                    Text("Saved changes apply to new connections of assigned machines.").font(.caption).foregroundStyle(
                        .secondary)
                }
                if !model.steps.isEmpty {
                    Section("Test results") { ForEach(model.steps) { ProxyTestStepRow(step: $0) } }
                }
                if let error = model.error { Text(error).foregroundStyle(.red) }
            }.formStyle(.grouped).disabled(model.isSaving || model.isTesting)
            Divider()
            HStack {
                Button(model.isTesting ? "Testing…" : "Test without saving") {
                    Task { await model.test(client: client) }
                }
                .disabled(model.isTesting || model.isSaving)
                Spacer()
                Button("Cancel") {
                    model.password = ""; model.urlInput = ""; dismiss()
                }.disabled(model.isSaving)
                Button("Save") { Task { if await model.save(client: client) { await onSaved(); dismiss() } } }
                    .keyboardShortcut(.defaultAction).disabled(model.isSaving || model.isTesting)
            }.padding()
        }.frame(width: 560, height: 700).interactiveDismissDisabled(model.isSaving || model.isTesting)
            .onChange(of: model.draft) { model.clearTestResults() }
            .onChange(of: model.password) { model.clearTestResults() }
            .onChange(of: model.urlInput) { model.clearTestResults() }
            .onChange(of: model.excludeText) { model.clearTestResults() }
    }
}

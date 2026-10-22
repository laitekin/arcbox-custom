import ArcBoxClient
import SwiftUI

struct MachineProxySection: View {
    let machine: String
    @Environment(\.arcboxClient) private var client
    @State private var library = ProxyLibraryModel()
    @State private var choice = ""
    @State private var savedChoice = ""
    @State private var effective = ""
    private enum Phase { case loading, loaded, failed }
    @State private var phase: Phase = .loading
    @State private var isSaving = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Outbound proxy").font(.system(size: 13, weight: .semibold)).accessibilityAddTraits(.isHeader)
            VStack(alignment: .leading, spacing: 10) {
                if phase == .loaded {
                    Picker("Proxy for this machine", selection: $choice) {
                        Text("Follow default").tag("")
                        Text("Direct").tag("direct")
                        ForEach(library.profiles) { Text($0.menuLabel).tag($0.id) }
                        if !savedChoice.isEmpty && savedChoice != "direct"
                            && !library.profiles.contains(where: { $0.id == savedChoice })
                        {
                            Text("Saved proxy unavailable").tag(savedChoice)
                        }
                    }.disabled(isSaving)
                    Text("In use: \(effective)").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    Text("Applies to new connections without restarting the machine.").font(.caption).foregroundStyle(
                        .secondary)
                    HStack {
                        Button("Refresh") { Task { await load() } }.disabled(isSaving)
                        Spacer()
                        Button("Revert") { choice = savedChoice }.disabled(choice == savedChoice || isSaving)
                        Button(isSaving ? "Applying…" : "Apply") { Task { await save() } }.disabled(
                            choice == savedChoice || isSaving)
                    }
                } else if phase == .loading {
                    ProgressView().controlSize(.small)
                }
                if let error {
                    Text(error).font(.caption).foregroundStyle(.red)
                    if phase == .failed { Button("Try Again") { Task { await load() } } }
                }
            }.padding(12).infoSectionStyle()
        }.task(id: machine) { await load() }
    }

    private func load() async {
        guard let client else { error = "ArcBox daemon is unavailable."; phase = .failed; return }
        phase = .loading
        error = nil
        await library.load(client: client)
        var request = Arcbox_V1_MachineProxyRequest()
        request.machine = machine
        do {
            let result = try await client.proxyLibrary.getMachineProxy(
                request, options: ArcBoxClient.defaultCallOptions)
            apply(result)
        } catch { self.error = ArcBoxClient.userMessage(for: error); phase = .failed }
    }

    private func save() async {
        guard let client else { return }
        isSaving = true
        defer { isSaving = false }
        var request = Arcbox_V1_SetMachineProxyRequest()
        request.machine = machine
        request.choice = choice
        do {
            let result = try await client.proxyLibrary.setMachineProxy(
                request, options: ArcBoxClient.defaultCallOptions)
            error = nil
            apply(result)
        } catch { self.error = ArcBoxClient.userMessage(for: error) }
    }

    private func apply(_ result: Arcbox_V1_MachineProxyAssignment) {
        choice = result.choice
        savedChoice = result.choice
        let settings = EgressProxySettings(proto: result.effective)
        effective = settings.effective.isEmpty ? "Direct" : settings.effective.joined(separator: ", ")
        phase = .loaded
    }
}

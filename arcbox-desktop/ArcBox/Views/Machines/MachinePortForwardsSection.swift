import AppKit
import ArcBoxClient
import SwiftUI

/// Machine Info › Forwarded ports: ports on this Mac's 127.0.0.1 that reach
/// a port of the machine. It is how the Mac reaches an isolated machine,
/// which has no address of its own on the Mac, for VNC say.
struct MachinePortForwardsSection: View {
    let machine: MachineViewModel
    @Environment(MachinesViewModel.self) private var vm
    @Environment(\.arcboxClient) private var client
    @State private var guestPort = ""
    @State private var hostPort = ""
    @State private var isSaving = false
    @State private var isOpeningDesktop = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Forwarded ports")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)

            VStack(alignment: .leading, spacing: 10) {
                if machine.portForwards.isEmpty {
                    Text("None. Forward a port to reach a service in the machine from this Mac.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                ForEach(machine.portForwards) { forward in
                    HStack {
                        Text(
                            verbatim:
                                "localhost:\(forward.hostPort) → \(forward.guestPort)\(forward.udp ? " (UDP)" : "")"
                        )
                        .font(.body.monospacedDigit())
                        .textSelection(.enabled)
                        Spacer()
                        if !forward.udp {
                            Button("Open in Browser") { Task { await openDesktop(forward) } }
                                .disabled(!machine.isRunning || client == nil || isOpeningDesktop)
                        }
                        if !forward.udp && (forward.guestPort == 5900 || forward.guestPort == 5901) {
                            Button("Open VNC") { open("vnc://localhost:\(forward.hostPort)") }
                        }
                        Button("Copy") { copy("localhost:\(forward.hostPort)") }
                        Button("Remove", role: .destructive) {
                            Task { await save(machine.portForwards.filter { $0 != forward }) }
                        }
                    }
                }

                HStack {
                    TextField("Machine port", text: $guestPort, prompt: Text("5901"))
                        .frame(width: 110)
                    TextField("Mac port", text: $hostPort, prompt: Text("Same"))
                        .frame(width: 90)
                    Button("Add") { Task { await add() } }
                        .disabled(UInt16(guestPort) == nil || isSaving)
                    Button("Add VNC (5901)") {
                        guestPort = "5901"
                        hostPort = ""
                        Task { await add() }
                    }
                    .disabled(isSaving || machine.portForwards.contains { $0.guestPort == 5901 })
                    if isSaving {
                        ProgressView().controlSize(.small)
                    }
                }

                if let error {
                    Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                }
                Text(
                    "Bound while the machine runs. The service must listen on all interfaces (for TigerVNC, start it with -localhost no)."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .infoSectionStyle()
        }
        .task(id: machine.id) { error = nil }
        .onChange(of: machine.isRunning) { _, running in
            if !running { WebDesktopServer.shared.stop(machine: machine.id) }
        }
    }

    private func openDesktop(_ forward: MachinePortForwardViewModel) async {
        guard let client else { return }
        isOpeningDesktop = true
        defer { isOpeningDesktop = false }
        do {
            let url = try await WebDesktopServer.shared.open(machine: machine, forward: forward, client: client)
            NSWorkspace.shared.open(url)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func add() async {
        guard let guest = UInt16(guestPort), guest != 0 else { return }
        let host = UInt16(hostPort) ?? guest
        var forwards = machine.portForwards.filter { $0.hostPort != host || $0.udp }
        forwards.append(MachinePortForwardViewModel(hostPort: host, guestPort: guest))
        if await save(forwards) {
            guestPort = ""
            hostPort = ""
        }
    }

    @discardableResult
    private func save(_ forwards: [MachinePortForwardViewModel]) async -> Bool {
        isSaving = true
        defer { isSaving = false }
        error = await vm.setMachinePortForwards(machine.id, forwards, client: client)
        return error == nil
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func open(_ url: String) {
        if let url = URL(string: url) {
            NSWorkspace.shared.open(url)
        }
    }
}

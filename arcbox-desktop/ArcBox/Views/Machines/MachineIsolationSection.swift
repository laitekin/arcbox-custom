import ArcBoxClient
import GRPCCore
import SwiftUI

/// Machine Info › Isolation: what the machine can reach of the Mac, the
/// switch to isolate a stopped machine or lift its isolation, the identity
/// it presents, and a check that probes the isolation from inside.
struct MachineIsolationSection: View {
    let machine: MachineViewModel
    @Environment(MachinesViewModel.self) private var vm
    @Environment(\.arcboxClient) private var client
    @State private var isSaving = false
    @State private var timezone = ""
    @State private var locale = ""
    @State private var isChecking = false
    @State private var results: [IsolationCheck.Result] = []
    @State private var checkError: String?

    private var isStopped: Bool {
        (machine.state == .stopped || machine.state == .created) && !machine.isBusy
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Isolation")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)

            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    Image(systemName: machine.isolated ? "lock.shield.fill" : "lock.open")
                        .foregroundStyle(machine.isolated ? Color.green : Color.secondary)
                        .accessibilityHidden(true)
                    Text(machine.isolated ? "Isolated from your Mac" : "Not isolated")
                        .fontWeight(.medium)
                    Spacer()
                    if isSaving {
                        ProgressView().controlSize(.small)
                    }
                    Button(machine.isolated ? "Lift Isolation" : "Isolate") {
                        Task { await toggle() }
                    }
                    .disabled(!isStopped || isSaving)
                }

                VStack(alignment: .leading, spacing: 4) {
                    ForEach(MachineIsolation.items(isolated: machine.isolated, mountCount: machine.mounts.count)) {
                        item in
                        Label(item.text, systemImage: symbol(item.status))
                            .font(.caption)
                            .foregroundStyle(color(item.status))
                    }
                }

                Text(
                    isStopped
                        ? "Isolating needs a saved proxy (Outbound proxy below). Takes effect at the next start."
                        : "Stop the machine to change its isolation, time zone or locale."
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                Divider()
                identity

                if machine.isRunning {
                    Divider()
                    selfCheck
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .infoSectionStyle()
        }
        .task(id: "\(machine.id)/\(machine.timezone)/\(machine.locale)") {
            timezone = machine.timezone
            locale = machine.locale
        }
        .task(id: machine.id) {
            results = []
            checkError = nil
        }
    }

    // MARK: - Identity

    private var identity: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !machine.macAddress.isEmpty {
                LabeledContent("Network card", value: machine.macAddress)
                    .textSelection(.enabled)
            }
            HStack {
                TextField("Time zone", text: $timezone, prompt: Text("Image default, e.g. Europe/Berlin"))
                TextField("Locale", text: $locale, prompt: Text("Image default, e.g. en_US.UTF-8"))
                Button("Save") {
                    Task {
                        isSaving = true
                        defer { isSaving = false }
                        await vm.setMachineIdentity(
                            machine.id,
                            timezone: timezone.trimmingCharacters(in: .whitespaces),
                            locale: locale.trimmingCharacters(in: .whitespaces),
                            client: client
                        )
                    }
                }
                .disabled(
                    !isStopped || isSaving || (timezone == machine.timezone && locale == machine.locale))
            }
            .disabled(!isStopped)
            Text("Match these to your proxy's location so the machine's clock and language agree with its address.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Self-check

    private var selfCheck: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Isolation check").fontWeight(.medium)
                Spacer()
                if isChecking {
                    ProgressView().controlSize(.small)
                }
                Button(isChecking ? "Checking…" : "Run Check") { Task { await runCheck() } }
                    .disabled(isChecking || client == nil)
            }
            if results.isEmpty && checkError == nil {
                Text(
                    "Tries, from inside the machine, to reach your Mac's files, your Mac, your network and the internet directly."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            ForEach(results) { result in
                Label {
                    Text("\(result.title): \(result.detail)").textSelection(.enabled)
                } icon: {
                    Image(systemName: checkSymbol(result.status)).foregroundStyle(checkColor(result.status))
                }
                .font(.caption)
            }
            if let checkError {
                Text(checkError).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            }
        }
    }

    private func runCheck() async {
        guard let client else { return }
        isChecking = true
        defer { isChecking = false }
        results = []
        checkError = nil
        var options = ArcBoxClient.defaultCallOptions
        options.timeout = .seconds(90)
        do {
            let output = try await MachineExec.run(
                IsolationCheck.request(machine: machine.id), client: client, options: options)
            results = IsolationCheck.parse(String(decoding: output.stdout, as: UTF8.self))
            if results.isEmpty {
                checkError = "The check printed nothing: " + String(decoding: output.stderr, as: UTF8.self)
            }
        } catch {
            checkError = ArcBoxClient.userMessage(for: error)
        }
    }

    private func toggle() async {
        isSaving = true
        defer { isSaving = false }
        await vm.setMachineIsolation(machine.id, isolated: !machine.isolated, client: client)
    }

    private func symbol(_ status: MachineIsolation.Item.Status) -> String {
        switch status {
        case .covered: "checkmark.circle.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .notCovered: "circle.dashed"
        }
    }

    private func color(_ status: MachineIsolation.Item.Status) -> Color {
        switch status {
        case .covered: .green
        case .warning: .orange
        case .notCovered: .secondary
        }
    }

    private func checkSymbol(_ status: IsolationCheck.Result.Status) -> String {
        switch status {
        case .pass: "checkmark.circle.fill"
        case .fail: "xmark.octagon.fill"
        case .info: "info.circle"
        }
    }

    private func checkColor(_ status: IsolationCheck.Result.Status) -> Color {
        switch status {
        case .pass: .green
        case .fail: .red
        case .info: .secondary
        }
    }
}

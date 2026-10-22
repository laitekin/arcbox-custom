import ArcBoxClient
import SwiftUI

/// Machine Info › Isolation: what the machine can reach of the Mac, and the
/// switch to isolate a stopped machine or lift its isolation.
struct MachineIsolationSection: View {
    let machine: MachineViewModel
    @Environment(MachinesViewModel.self) private var vm
    @Environment(\.arcboxClient) private var client
    @State private var isSaving = false

    private var canChange: Bool {
        (machine.state == .stopped || machine.state == .created) && !machine.isBusy && !isSaving
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
                    .disabled(!canChange)
                }

                VStack(alignment: .leading, spacing: 4) {
                    ForEach(MachineIsolation.items(isolated: machine.isolated, mountCount: machine.mounts.count)) {
                        item in
                        Label(item.text, systemImage: symbol(item.status))
                            .font(.caption)
                            .foregroundStyle(color(item.status))
                    }
                }

                if !canChange && !isSaving {
                    Text("Stop the machine to change its isolation.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else if canChange {
                    Text("Takes effect when the machine next starts.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .infoSectionStyle()
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
}

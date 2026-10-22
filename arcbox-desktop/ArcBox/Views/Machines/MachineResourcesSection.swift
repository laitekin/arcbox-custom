import ArcBoxClient
import SwiftUI

/// Editable CPU and memory limits, the guest's live usage, and a restart
/// prompt when a saved change hasn't taken effect yet.
struct MachineResourcesSection: View {
    let machine: MachineViewModel
    @Environment(MachinesViewModel.self) private var vm
    @Environment(\.arcboxClient) private var client
    @State private var draft = MachineResourceDraft(cpus: 1, memoryGiB: 1)
    @State private var limits = MachineResourceLimits.thisMac
    @State private var usage: GuestResourceUsage?
    @State private var isSaving = false

    private static let usageRefresh: Duration = .seconds(5)

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Resources")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)

            VStack(alignment: .leading, spacing: 10) {
                Stepper(value: $draft.cpus, in: limits.cpuRange) {
                    LabeledContent("CPU limit", value: "\(draft.cpus) of \(limits.hostCPUs) cores")
                }
                Stepper(value: $draft.memoryGiB, in: limits.memoryRange) {
                    LabeledContent("Memory limit", value: "\(draft.memoryGiB) of \(limits.hostMemoryGiB) GB")
                }
                LabeledContent("Disk", value: "\(machine.diskGB) GB")

                if let usage, machine.isRunning {
                    Divider()
                    VStack(alignment: .leading, spacing: 4) {
                        LabeledContent("Memory in use", value: usage.memoryDisplay)
                        ProgressView(value: usage.memoryFraction)
                            .accessibilityLabel("Memory in use")
                    }
                    if let cpu = usage.cpuDisplay {
                        LabeledContent("CPU activity", value: cpu)
                    }
                }

                if restartPending {
                    restartBanner
                }

                Text(
                    "Limits are ceilings: the machine uses CPU only while busy and takes memory as it needs it, "
                        + "up to the limit. New limits apply at the next start."
                )
                .font(.caption)
                .foregroundStyle(.secondary)

                HStack {
                    Spacer()
                    Button("Revert") {
                        draft = MachineResourceDraft(cpus: machine.cpuCores, memoryGiB: machine.memoryGB)
                    }
                    .disabled(!draft.isDirty || isSaving)
                    Button(isSaving ? "Saving…" : "Save") { Task { await save() } }
                        .disabled(!draft.isDirty || isSaving || machine.isBusy)
                }
            }
            .padding(12)
            .infoSectionStyle()
        }
        .task(id: "\(machine.id)/\(machine.cpuCores)/\(machine.memoryGB)") {
            draft = MachineResourceDraft(cpus: machine.cpuCores, memoryGiB: machine.memoryGB)
        }
        .task(id: "\(machine.id)/\(machine.isRunning)") {
            usage = nil
            guard machine.isRunning else { return }
            while !Task.isCancelled {
                await refreshUsage()
                try? await Task.sleep(for: Self.usageRefresh)
            }
        }
    }

    private var restartPending: Bool {
        guard machine.isRunning else { return false }
        if vm.resourceRestartPending.contains(machine.id) { return true }
        return usage?.bootedWithDifferentCPUs(than: machine.cpuCores) ?? false
    }

    private var restartBanner: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "arrow.clockwise.circle.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            Text("Saved. The machine is still running with its previous limits until it restarts.")
                .font(.caption)
            Spacer()
            Button(machine.isBusy ? "Restarting…" : "Restart Now") {
                Task { await vm.restartMachine(machine.id, client: client) }
            }
            .disabled(machine.isBusy)
        }
        .padding(8)
        .background(.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
    }

    private func save() async {
        isSaving = true
        defer { isSaving = false }
        guard let response = await vm.setMachineResources(machine.id, draft: draft, client: client) else {
            return
        }
        if response.hostCpus > 0, response.hostMemory > 0 {
            limits = MachineResourceLimits(hostCPUs: response.hostCpus, hostMemoryGiB: response.hostMemory >> 30)
        }
        draft = MachineResourceDraft(
            cpus: response.cpus,
            memoryGiB: UInt32(clamping: (response.memory + (1 << 29)) >> 30)
        )
    }

    private func refreshUsage() async {
        guard let client else { return }
        var request = Arcbox_V1_MachineAgentRequest()
        request.id = machine.id
        do {
            let info = try await client.machines.getSystemInfo(request, options: ArcBoxClient.defaultCallOptions)
            usage = GuestResourceUsage(from: info)
        } catch {
            // The agent is unreachable while the machine boots; keep the last
            // reading and try again on the next tick.
        }
    }
}

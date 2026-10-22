import ArcBoxClient
import Foundation

/// Host ceilings for a machine's CPU and memory limits.
///
/// Seeded from this Mac and replaced by the values the daemon reports in
/// `SetMachineResourcesResponse`, which are the ones it enforces.
struct MachineResourceLimits: Equatable {
    var hostCPUs: UInt32
    var hostMemoryGiB: UInt64

    /// The daemon's floor for memory is 512 MiB; the editor works in whole GiB.
    static let minimumMemoryGiB: UInt64 = 1

    static var thisMac: MachineResourceLimits {
        MachineResourceLimits(
            hostCPUs: UInt32(max(ProcessInfo.processInfo.activeProcessorCount, 1)),
            hostMemoryGiB: max(ProcessInfo.processInfo.physicalMemory >> 30, minimumMemoryGiB)
        )
    }

    var cpuRange: ClosedRange<Int> { 1...Int(max(hostCPUs, 1)) }
    var memoryRange: ClosedRange<Int> {
        Int(Self.minimumMemoryGiB)...Int(max(hostMemoryGiB, Self.minimumMemoryGiB))
    }
}

/// An edit of a machine's CPU and memory limits.
struct MachineResourceDraft: Equatable {
    var cpus: Int
    var memoryGiB: Int
    let savedCPUs: Int
    let savedMemoryGiB: Int

    init(cpus: UInt32, memoryGiB: UInt32) {
        self.cpus = Int(cpus)
        self.memoryGiB = Int(memoryGiB)
        self.savedCPUs = Int(cpus)
        self.savedMemoryGiB = Int(memoryGiB)
    }

    var isDirty: Bool { cpus != savedCPUs || memoryGiB != savedMemoryGiB }

    /// The request for the changed fields only; an unchanged field is sent as
    /// 0, which the daemon reads as "keep the current value".
    func request(machine: String) -> Arcbox_V1_SetMachineResourcesRequest {
        var request = Arcbox_V1_SetMachineResourcesRequest()
        request.id = machine
        if cpus != savedCPUs {
            request.cpus = UInt32(cpus)
        }
        if memoryGiB != savedMemoryGiB {
            request.memory = UInt64(memoryGiB) << 30
        }
        return request
    }
}

/// What a running machine is using, as its guest agent reports it.
struct GuestResourceUsage: Equatable {
    /// Memory the guest kernel manages; slightly below the configured limit
    /// because firmware and the kernel itself reserve some.
    let totalMemory: UInt64
    let usedMemory: UInt64
    /// CPUs the guest booted with.
    let cpuCount: UInt32
    /// 1-minute load average: runnable tasks, so `cpuCount` means saturated.
    let load1: Double?

    init(from info: Arcbox_V1_MachineSystemInfo) {
        totalMemory = info.totalMemory
        usedMemory = info.totalMemory - min(info.availableMemory, info.totalMemory)
        cpuCount = info.cpuCount
        load1 = info.loadAverage.first
    }

    var memoryFraction: Double {
        totalMemory == 0 ? 0 : Double(usedMemory) / Double(totalMemory)
    }

    var memoryDisplay: String {
        "\(Self.format(usedMemory)) of \(Self.format(totalMemory))"
    }

    var cpuDisplay: String? {
        guard let load1 else { return nil }
        return String(format: "load %.2f on %u CPUs", load1, cpuCount)
    }

    /// The guest booted with a different CPU count than is configured, so a
    /// saved change is waiting for a restart. Memory can't be compared this
    /// way: the guest always sees less than its limit.
    func bootedWithDifferentCPUs(than configured: UInt32) -> Bool {
        cpuCount != 0 && cpuCount != configured
    }

    private static func format(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .memory)
    }
}

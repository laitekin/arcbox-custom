import ArcBoxClient
import Foundation

/// Machine state representation.
///
/// Mirrors the daemon's wire strings (`format!("{:?}").to_lowercase()` of
/// arcbox-core's `MachineState`).
enum MachineState: String, CaseIterable, Identifiable {
    case created
    case starting
    case running
    case stopping
    case stopped

    init(apiState: String) {
        self = MachineState(rawValue: apiState) ?? .stopped
    }

    var id: String { rawValue }

    var label: String { rawValue.capitalized }

    var isRunning: Bool { self == .running }
}

/// Linux distribution info
struct DistroInfo: Hashable {
    let name: String
    let version: String
    let displayName: String

    /// Build from wire fields; empty distro means a plain (non-distro) VM.
    init(distro: String, version: String) {
        self.name = distro
        self.version = version
        self.displayName = distro.isEmpty ? "Linux" : distro.capitalized
    }
}

/// A directory shared into a machine.
struct MachineMountViewModel: Hashable, Identifiable {
    let hostPath: String
    let guestPath: String
    let readOnly: Bool

    var id: String { "\(hostPath):\(guestPath)" }

    init(from mount: Arcbox_V1_DirectoryMount) {
        self.hostPath = mount.hostPath
        self.guestPath = mount.guestPath
        self.readOnly = mount.readonly
    }
}

/// A port on the Mac's loopback forwarded into a machine.
struct MachinePortForwardViewModel: Hashable, Identifiable {
    var hostPort: UInt16
    var guestPort: UInt16
    var udp: Bool

    var id: String { "\(hostPort)/\(udp ? "udp" : "tcp")" }

    init(hostPort: UInt16, guestPort: UInt16, udp: Bool = false) {
        self.hostPort = hostPort
        self.guestPort = guestPort
        self.udp = udp
    }

    init?(from forward: Arcbox_V1_MachinePortForward) {
        guard let host = UInt16(exactly: forward.hostPort), let guest = UInt16(exactly: forward.guestPort) else {
            return nil
        }
        self.init(hostPort: host, guestPort: guest, udp: forward.protocol.lowercased() == "udp")
    }

    var proto: Arcbox_V1_MachinePortForward {
        var forward = Arcbox_V1_MachinePortForward()
        forward.hostPort = UInt32(hostPort)
        forward.guestPort = UInt32(guestPort)
        forward.protocol = udp ? "udp" : "tcp"
        return forward
    }
}

/// Machine view model for UI display
struct MachineViewModel: Identifiable, Hashable {
    let id: String
    let name: String
    var distro: DistroInfo
    var state: MachineState
    var cpuCores: UInt32
    var memoryGB: UInt32
    var diskGB: UInt32
    var architecture: String
    var ipAddress: String?
    var createdAt: Date
    /// Isolated from the Mac: no share of its files (see `SetIsolation`).
    var isolated: Bool

    // Detail fields populated by Inspect (empty until loaded).
    var gateway: String = ""
    var macAddress: String = ""
    var dnsServers: [String] = []
    var mounts: [MachineMountViewModel] = []
    var startedAt: Date?
    /// Time zone and locale the machine presents; empty keeps the image's.
    var timezone: String = ""
    var locale: String = ""
    var portForwards: [MachinePortForwardViewModel] = []

    /// A lifecycle RPC is in flight for this machine.
    var isTransitioning: Bool = false

    var isRunning: Bool { state.isRunning }

    /// A lifecycle transition is under way — either an RPC this session issued
    /// (`isTransitioning`) or a daemon-reported starting/stopping state. The
    /// row shows a spinner instead of a start/stop button while busy, so the
    /// user can't fire a second lifecycle RPC into a racing one.
    var isBusy: Bool {
        isTransitioning || state == .starting || state == .stopping
    }

    var resourcesDisplay: String {
        "\(cpuCores) cores, \(memoryGB) GB RAM, \(diskGB) GB disk"
    }

    init(from summary: Arcbox_V1_MachineSummary) {
        self.id = summary.id
        self.name = summary.name
        self.distro = DistroInfo(distro: summary.distro, version: summary.distroVersion)
        self.state = MachineState(apiState: summary.state)
        self.cpuCores = summary.cpus
        self.memoryGB = Self.gigabytes(summary.memory)
        self.diskGB = Self.gigabytes(summary.diskSize)
        self.architecture = ""
        self.ipAddress = summary.ipAddress.isEmpty ? nil : summary.ipAddress
        self.createdAt = Date(timeIntervalSince1970: TimeInterval(summary.created))
        self.isolated = summary.isolated
    }

    /// Merge Inspect-only fields into a summary-built view model.
    mutating func applyDetails(from info: Arcbox_V1_MachineInfo) {
        architecture = info.hardware.arch
        gateway = info.network.gateway
        macAddress = info.network.macAddress
        dnsServers = info.network.dnsServers
        mounts = info.mounts.map(MachineMountViewModel.init(from:))
        isolated = info.isolated
        timezone = info.timezone
        locale = info.locale
        portForwards = info.portForwards.compactMap(MachinePortForwardViewModel.init(from:))
        if info.hasStartedAt, info.startedAt.seconds > 0 {
            startedAt = Date(timeIntervalSince1970: TimeInterval(info.startedAt.seconds))
        }
    }

    /// Preserve detail fields across a list refresh, which only carries
    /// summary data.
    mutating func preserveDetailFrom(_ existing: MachineViewModel) {
        architecture = existing.architecture
        gateway = existing.gateway
        macAddress = existing.macAddress
        dnsServers = existing.dnsServers
        mounts = existing.mounts
        startedAt = existing.startedAt
        timezone = existing.timezone
        locale = existing.locale
        portForwards = existing.portForwards
    }

    private static func gigabytes(_ bytes: UInt64) -> UInt32 {
        UInt32((bytes + (1 << 29)) >> 30)
    }
}

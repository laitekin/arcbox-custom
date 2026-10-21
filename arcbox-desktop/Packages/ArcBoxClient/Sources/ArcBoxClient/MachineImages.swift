import Foundation

/// A machine image in the daemon's local registry.
public struct LocalMachineImage: Identifiable, Equatable, Sendable {
    public var id: String { reference }
    /// `name@version`, what `CreateMachineRequest.image` takes.
    public var reference: String { "\(name)@\(version)" }

    public let name: String
    public let version: String
    public let distro: String
    public let release: String
    public let releaseTitle: String
    public let arch: String
    /// Imported on this Mac rather than pulled from the image source.
    public let imported: Bool
    public let source: String
    public let size: UInt64
    public let sha256: String
    /// Machines that boot this image.
    public let machines: [String]

    public init(proto: Arcbox_V1_MachineImageInfo) {
        name = proto.name
        version = proto.version
        distro = proto.distro
        release = proto.release
        releaseTitle = proto.releaseTitle
        arch = proto.arch
        imported = proto.imported
        source = proto.source
        size = proto.size
        sha256 = proto.sha256
        machines = proto.machines
    }

    /// What the image is, for a picker row (`Ubuntu 24.04 LTS`, `alpine 3.24`).
    public var displayTitle: String {
        releaseTitle.isEmpty || releaseTitle == release ? "\(distro) \(release)" : releaseTitle
    }
}

/// Where an import reads its root filesystem from.
public enum MachineImageImportSource: Equatable, Sendable {
    /// A rootfs archive or squashfs file on this Mac.
    case file(URL)
    /// An image in ArcBox's Docker engine (`ubuntu:24.04`). With `addInit`
    /// the daemon first builds a derived image with systemd (or OpenRC on
    /// Alpine) installed, imports that, and removes it.
    case docker(String, addInit: Bool = false)
}

/// One step of an import, as the daemon reports it.
public enum MachineImageImportPhase: Equatable, Sendable {
    case building
    case exporting
    case reading(bytes: UInt64)
    case checking
    case writing
    case registering
    case done(LocalMachineImage, warnings: [String])

    public init?(proto: Arcbox_V1_ImportMachineImageEvent) {
        switch proto.phase {
        case .building: self = .building
        case .exporting: self = .exporting
        case .reading: self = .reading(bytes: proto.bytes)
        case .checking: self = .checking
        case .writing: self = .writing
        case .registering: self = .registering
        case .done: self = .done(LocalMachineImage(proto: proto.image), warnings: proto.warnings)
        case .unspecified, .UNRECOGNIZED: return nil
        }
    }

    public var label: String {
        switch self {
        case .building: "Installing an init system into the Docker image…"
        case .exporting: "Exporting the Docker image…"
        case .reading(let bytes):
            "Reading the root filesystem… \(ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file))"
        case .checking: "Checking init and architecture…"
        case .writing: "Compressing the image…"
        case .registering: "Registering…"
        case .done: "Done"
        }
    }
}

/// Image names the daemon accepts: 1–63 lowercase letters, digits, `.`,
/// `-` or `_`, starting with a letter or digit.
public func isValidMachineImageName(_ name: String) -> Bool {
    guard (1...63).contains(name.count), let first = name.unicodeScalars.first else { return false }
    let lowerOrDigit = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789")
    let allowed = lowerOrDigit.union(CharacterSet(charactersIn: ".-_"))
    return lowerOrDigit.contains(first) && name.unicodeScalars.allSatisfy { allowed.contains($0) }
}

/// An image name for an imported file: `ubuntu-rootfs.tar.gz` →
/// `ubuntu-rootfs`, lowercased and limited to the characters a name allows.
public func suggestedMachineImageName(forFile fileName: String) -> String {
    var stem = fileName.lowercased()
    for suffix in [".tar.gz", ".tar.xz", ".tar.zst", ".tgz", ".tar", ".squashfs", ".sqfs"]
    where stem.hasSuffix(suffix) {
        stem.removeLast(suffix.count)
        break
    }
    let allowed = Set("abcdefghijklmnopqrstuvwxyz0123456789.-_")
    let cleaned = String(stem.map { allowed.contains($0) ? $0 : "-" })
        .trimmingCharacters(in: CharacterSet(charactersIn: ".-_"))
    return String(cleaned.prefix(63))
}

extension MachineImageImportSource {
    /// The import request for this source under `name`.
    public func request(name: String) -> Arcbox_V1_ImportMachineImageRequest {
        var request = Arcbox_V1_ImportMachineImageRequest()
        request.name = name
        switch self {
        case .file(let url): request.path = url.path
        case .docker(let image, let addInit):
            request.dockerImage = image
            request.addInit = addInit
        }
        return request
    }
}

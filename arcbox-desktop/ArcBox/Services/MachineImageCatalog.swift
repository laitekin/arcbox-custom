import ArcBoxClient
import Foundation
import os

/// A distro available for machine creation, with its published releases.
struct MachineDistroOption: Identifiable, Hashable {
    let distro: String
    let releases: [String]

    var id: String { distro }

    var displayName: String { distro.capitalized }
}

/// Fetches the published machine image catalog from the ArcBox CDN.
///
/// The daemon resolves `(distro, release)` against the same index at create
/// time, so this only drives the picker UI; a stale or unreachable catalog
/// degrades to a built-in snapshot of the published streams.
enum MachineImageCatalog {
    private static let indexURL = URL(string: "https://image.arcboxcdn.com/linux/index.json")!

    /// Snapshot of the published index, used when the CDN is unreachable.
    static let fallback: [MachineDistroOption] = [
        MachineDistroOption(distro: "ubuntu", releases: ["resolute", "noble"]),
        MachineDistroOption(distro: "debian", releases: ["forky", "trixie"]),
        MachineDistroOption(distro: "fedora", releases: ["44", "43"]),
        MachineDistroOption(distro: "alpine", releases: ["3.24", "3.23"]),
        MachineDistroOption(distro: "archlinux", releases: ["current"]),
    ]

    /// Preferred picker ordering; unknown distros sort after these.
    private static let distroOrder = ["ubuntu", "debian", "fedora", "alpine", "archlinux"]

    /// Fetch distros published for the host architecture: from the daemon,
    /// which reads the configured image source through the egress proxy;
    /// then directly from the CDN; then the built-in snapshot.
    static func fetch(client: ArcBoxClient?) async -> [MachineDistroOption] {
        if let client {
            do {
                let listing = try await client.machineImages.listRemote(
                    .init(), options: ArcBoxClient.defaultCallOptions)
                var releasesByDistro: [String: Set<String>] = [:]
                for image in listing.images {
                    releasesByDistro[image.distro, default: []].insert(image.release)
                }
                let options = options(from: releasesByDistro)
                if !options.isEmpty { return options }
            } catch {
                Log.machine.warning(
                    "Daemon image listing failed, reading the CDN: \(error.localizedDescription, privacy: .private)"
                )
            }
        }
        return await fetch()
    }

    /// Fetch distros published for the host architecture from the CDN.
    static func fetch() async -> [MachineDistroOption] {
        var request = URLRequest(url: indexURL)
        request.timeoutInterval = 10
        do {
            let (data, _) = try await URLSession.shared.data(for: request)
            let index = try JSONDecoder().decode(Index.self, from: data)
            let options = distroOptions(from: index)
            return options.isEmpty ? fallback : options
        } catch {
            Log.machine.warning(
                "Machine image catalog fetch failed, using fallback: \(error.localizedDescription, privacy: .private)"
            )
            return fallback
        }
    }

    private static func distroOptions(from index: Index) -> [MachineDistroOption] {
        var releasesByDistro: [String: Set<String>] = [:]
        for stream in index.images.values where stream.arch == hostArch {
            releasesByDistro[stream.distro, default: []].insert(stream.release)
        }
        return options(from: releasesByDistro)
    }

    private static func options(from releasesByDistro: [String: Set<String>]) -> [MachineDistroOption] {
        let options = releasesByDistro.map { distro, releases in
            MachineDistroOption(
                distro: distro,
                releases: releases.sorted { $0.compare($1, options: .numeric) == .orderedDescending }
            )
        }
        return options.sorted { rank($0.distro) < rank($1.distro) }
    }

    private static func rank(_ distro: String) -> Int {
        distroOrder.firstIndex(of: distro) ?? distroOrder.count
    }

    /// Image index architecture for the host (`arm64`/`amd64`).
    private static var hostArch: String {
        #if arch(arm64)
            "arm64"
        #else
            "amd64"
        #endif
    }

    // MARK: - Index schema (machine-images schema_version 1)

    private struct Index: Decodable {
        let images: [String: Stream]
    }

    private struct Stream: Decodable {
        let distro: String
        let release: String
        let arch: String
    }
}

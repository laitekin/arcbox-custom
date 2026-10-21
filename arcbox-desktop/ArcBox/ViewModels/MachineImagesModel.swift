import ArcBoxClient
import Foundation
import OSLog

/// Drives the machine images sheet: the local registry and imports into it.
@MainActor
@Observable
final class MachineImagesModel {
    enum LoadState: Equatable {
        case waiting
        case loading
        case loaded
        case failed(String)
    }

    private(set) var loadState: LoadState = .waiting
    private(set) var images: [LocalMachineImage] = []
    /// The phase of the running import; `nil` when none runs.
    private(set) var importPhase: MachineImageImportPhase?
    private(set) var importWarnings: [String] = []
    private(set) var lastError: String?
    private(set) var removing: Set<String> = []

    var isImporting: Bool { importPhase != nil }

    func load(client: ArcBoxClient?) async {
        guard let client else {
            loadState = .waiting
            return
        }
        if loadState != .loaded { loadState = .loading }
        do {
            let response = try await client.machineImages.list(
                .init(), options: ArcBoxClient.defaultCallOptions)
            images = response.images.map(LocalMachineImage.init(proto:))
            loadState = .loaded
        } catch {
            Log.machine.error("Failed to list machine images: \(error.localizedDescription, privacy: .private)")
            loadState = .failed(ArcBoxClient.userMessage(for: error))
        }
    }

    /// Imports `source` as `name`, streaming progress; reloads the list when done.
    func importImage(_ source: MachineImageImportSource, name: String, client: ArcBoxClient?) async {
        guard let client, !isImporting else { return }
        lastError = nil
        importWarnings = []
        if case .docker = source {
            importPhase = .exporting
        } else {
            importPhase = .reading(bytes: 0)
        }
        do {
            try await client.machineImages.`import`(
                source.request(name: name), options: ArcBoxClient.machineImageImportCallOptions
            ) { response in
                for try await event in response.messages {
                    guard let phase = MachineImageImportPhase(proto: event) else { continue }
                    await MainActor.run { self.importPhase = phase }
                }
            }
            if case .done(_, let warnings) = importPhase {
                importWarnings = warnings
            }
        } catch {
            Log.machine.error("Machine image import failed: \(error.localizedDescription, privacy: .private)")
            lastError = ArcBoxClient.userMessage(for: error)
        }
        importPhase = nil
        await load(client: client)
    }

    func remove(_ image: LocalMachineImage, client: ArcBoxClient?) async {
        guard let client else { return }
        lastError = nil
        removing.insert(image.reference)
        var request = Arcbox_V1_RemoveMachineImageRequest()
        request.name = image.name
        request.version = image.version
        do {
            _ = try await client.machineImages.remove(request, options: ArcBoxClient.defaultCallOptions)
        } catch {
            lastError = ArcBoxClient.userMessage(for: error)
        }
        removing.remove(image.reference)
        await load(client: client)
    }
}

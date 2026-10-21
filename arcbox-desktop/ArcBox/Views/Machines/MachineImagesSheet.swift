import ArcBoxClient
import SwiftUI
import UniformTypeIdentifiers

/// Machines › Images: the local machine image registry, and importing a
/// root filesystem (archive, squashfs, or Docker image) into it.
struct MachineImagesSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.arcboxClient) private var client

    @State private var model = MachineImagesModel()
    @State private var importSource: ImportKind = .file
    @State private var importName = ""
    @State private var importFile: URL?
    @State private var dockerImage = ""
    @State private var addInit = true
    @State private var showFilePicker = false

    enum ImportKind: String, CaseIterable, Identifiable {
        case file = "Archive or squashfs"
        case docker = "Docker image"
        var id: String { rawValue }
    }

    private var importSourceValue: MachineImageImportSource? {
        switch importSource {
        case .file: importFile.map(MachineImageImportSource.file)
        case .docker:
            dockerImage.trimmingCharacters(in: .whitespaces).isEmpty
                ? nil : .docker(dockerImage.trimmingCharacters(in: .whitespaces), addInit: addInit)
        }
    }

    private var canImport: Bool {
        client != nil && !model.isImporting && importSourceValue != nil
            && isValidMachineImageName(importName)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Machine Images")
                    .font(.system(size: 13, weight: .semibold))
                Spacer()
                Button(
                    action: { dismiss() },
                    label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 12))
                            .foregroundStyle(AppColors.textSecondary)
                            .frame(width: AppMetrics.sheetCloseButton, height: AppMetrics.sheetCloseButton)
                    }
                )
                .buttonStyle(.plain)
                .accessibilityLabel("Close")
            }
            .padding(.horizontal, 16)
            .frame(height: AppMetrics.sheetTitleBarHeight)
            .overlay(alignment: .bottom) { Divider() }

            Form {
                imagesSection
                importSection
                if let error = model.lastError {
                    Section {
                        Label(error, systemImage: "exclamationmark.triangle")
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
        }
        .frame(width: 560, height: 560)
        .interactiveDismissDisabled(model.isImporting)
        .task(id: client != nil) {
            await model.load(client: client)
        }
        .fileImporter(
            isPresented: $showFilePicker,
            allowedContentTypes: [.data],
            allowsMultipleSelection: false
        ) { result in
            if case .success(let urls) = result, let url = urls.first {
                importFile = url
                if importName.isEmpty {
                    importName = suggestedMachineImageName(forFile: url.lastPathComponent)
                }
            }
        }
    }

    private var imagesSection: some View {
        Section("Local images") {
            switch model.loadState {
            case .waiting, .loading:
                ProgressView().controlSize(.small)
            case .failed(let message):
                Text(message).font(.system(size: 12)).foregroundStyle(.red)
            case .loaded where model.images.isEmpty:
                Text("No images yet. Creating a machine downloads one; importing adds your own.")
                    .font(.system(size: 12))
                    .foregroundStyle(AppColors.textSecondary)
            case .loaded:
                ForEach(model.images) { image in
                    MachineImageRow(
                        image: image,
                        isRemoving: model.removing.contains(image.reference)
                    ) {
                        Task { await model.remove(image, client: client) }
                    }
                }
            }
        }
    }

    private var importSection: some View {
        Section {
            Picker("Source", selection: $importSource) {
                ForEach(ImportKind.allCases) { kind in
                    Text(kind.rawValue).tag(kind)
                }
            }
            switch importSource {
            case .file:
                LabeledContent("File") {
                    HStack {
                        Text(importFile?.lastPathComponent ?? "None")
                            .foregroundStyle(AppColors.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Button("Choose…") { showFilePicker = true }
                    }
                }
            case .docker:
                TextField("Image", text: $dockerImage, prompt: Text("ubuntu:24.04"))
                Toggle("Install an init system", isOn: $addInit)
                    .help("Builds a temporary image with systemd (OpenRC on Alpine) installed, then imports it")
            }
            TextField("Name", text: $importName, prompt: Text("my-ubuntu"))
            if !importName.isEmpty && !isValidMachineImageName(importName) {
                Text("Use lowercase letters, digits, '.', '-' or '_'.")
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
            }
            Text(
                "The root filesystem must contain /sbin/init (systemd or OpenRC) built for this Mac. Docker images usually have none; keep \"Install an init system\" on to add one."
            )
            .font(.system(size: 11))
            .foregroundStyle(AppColors.textSecondary)

            HStack {
                if let phase = model.importPhase {
                    ProgressView().controlSize(.small)
                    Text(phase.label)
                        .font(.system(size: 12))
                        .foregroundStyle(AppColors.textSecondary)
                }
                Spacer()
                Button("Import") {
                    guard let source = importSourceValue else { return }
                    let name = importName
                    Task {
                        await model.importImage(source, name: name, client: client)
                        if model.lastError == nil {
                            importName = ""
                            importFile = nil
                            dockerImage = ""
                        }
                    }
                }
                .disabled(!canImport)
            }
            ForEach(model.importWarnings, id: \.self) { warning in
                Label(warning, systemImage: "exclamationmark.circle")
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
            }
        } header: {
            Text("Import")
        }
        .disabled(model.isImporting)
    }
}

private struct MachineImageRow: View {
    let image: LocalMachineImage
    let isRemoving: Bool
    let remove: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(image.name).font(.system(size: 13, weight: .medium))
                    Text(image.imported ? "Imported" : "Downloaded")
                        .font(.system(size: 10))
                        .padding(.horizontal, 5)
                        .background(.quaternary, in: Capsule())
                }
                Text(
                    "\(image.displayTitle) · \(image.version) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: image.size), countStyle: .file))"
                )
                .font(.system(size: 11))
                .foregroundStyle(AppColors.textSecondary)
                if !image.machines.isEmpty {
                    Text("Used by \(image.machines.joined(separator: ", "))")
                        .font(.system(size: 11))
                        .foregroundStyle(AppColors.textSecondary)
                }
            }
            Spacer()
            if isRemoving {
                ProgressView().controlSize(.small)
            } else {
                Button(role: .destructive, action: remove) {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Remove \(image.name)")
                .disabled(!image.machines.isEmpty)
                .help(image.machines.isEmpty ? "Remove" : "Remove the machines using it first")
            }
        }
    }
}

import AppKit
import ArcBoxClient
import SwiftUI

/// Buttons that copy text between the Mac and the machine's desktop
/// clipboard, for remote desktops whose own clipboard sharing doesn't work.
struct MachineClipboardSection: View {
    let machine: MachineViewModel
    @Environment(\.arcboxClient) private var client
    @State private var isWorking = false
    @State private var status: String?
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Clipboard")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)

            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Button("Send Mac Clipboard to Machine") { Task { await send() } }
                    Button("Copy Machine Clipboard to Mac") { Task { await receive() } }
                    if isWorking {
                        ProgressView().controlSize(.small)
                    }
                }
                .disabled(isWorking || !machine.isRunning || client == nil)

                if let error {
                    Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                } else if let status {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }

                Text(machine.isRunning ? Self.runningHint : "Start the machine to share its clipboard.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(12)
            .infoSectionStyle()
        }
        .task(id: machine.id) {
            status = nil
            error = nil
        }
    }

    private static let runningHint =
        "Works with any VNC client. The machine needs a desktop session and xclip or xsel. "
        + "In Linux, paste with Ctrl+V, or Ctrl+Shift+V in a terminal."

    private func send() async {
        guard let client else { return }
        isWorking = true
        defer { isWorking = false }
        let text = NSPasteboard.general.string(forType: .string)
        if let failure = MachineClipboard.validate(text) {
            show(.failure(failure))
            return
        }
        let result = await MachineClipboard.write(text ?? "", machine: machine.id, client: client)
        show(result.map { "Sent \(Self.describe(text ?? "")) to \(machine.name)." })
    }

    private func receive() async {
        guard let client else { return }
        isWorking = true
        defer { isWorking = false }
        let result = await MachineClipboard.read(machine: machine.id, client: client)
        if case .success(let text) = result {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
        }
        show(result.map { "Copied \(Self.describe($0)) to the Mac clipboard." })
    }

    private func show(_ result: Result<String, MachineClipboard.Failure>) {
        switch result {
        case .success(let message):
            status = message
            error = nil
        case .failure(let failure):
            error = failure.message
        }
    }

    private static func describe(_ text: String) -> String {
        text.count == 1 ? "1 character" : "\(text.count) characters"
    }
}

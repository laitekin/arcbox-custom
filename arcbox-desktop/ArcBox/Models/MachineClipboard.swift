import ArcBoxClient
import Foundation

/// Copies text between the Mac's clipboard and the clipboard of a machine's
/// X11 desktop (a VNC session, for example).
///
/// It goes through the guest agent rather than the remote-desktop protocol,
/// so it works whatever VNC client and server are in use. In the guest it
/// needs an X display under `/tmp/.X11-unix` and `xclip` or `xsel`.
enum MachineClipboard {
    /// Larger text would overflow a single environment string in the guest.
    static let maxBytes = 64 * 1024

    static let noDisplayExit: Int32 = 70
    static let noToolExit: Int32 = 71

    /// Runs as root. `$1` is `get` or `set`; `set` reads base64 text from
    /// `ARCBOX_CLIPBOARD`. It picks the highest-numbered X display (VNC
    /// servers usually take :1 and up) and talks to it as the account that
    /// owns it, with that account's Xauthority.
    static let script = #"""
        mode="$1"
        display=$(ls /tmp/.X11-unix 2>/dev/null | sed -n 's/^X\([0-9][0-9]*\)$/\1/p' | sort -n | tail -n 1)
        [ -n "$display" ] || exit 70
        owner=$(stat -c %U "/tmp/.X11-unix/X$display" 2>/dev/null || echo root)
        home=$(getent passwd "$owner" | cut -d: -f6)
        [ -n "$home" ] || home=/root
        if command -v xclip >/dev/null 2>&1; then
          get='xclip -selection clipboard -o'; put='xclip -selection clipboard -i'
        elif command -v xsel >/dev/null 2>&1; then
          get='xsel --clipboard --output'; put='xsel --clipboard --input'
        else
          exit 71
        fi
        x="DISPLAY=:$display HOME=$home"
        [ -f "$home/.Xauthority" ] && x="$x XAUTHORITY=$home/.Xauthority"
        as_owner() {
          if [ "$owner" = root ] || [ "$(id -u)" != 0 ]; then sh -c "$1"
          elif command -v runuser >/dev/null 2>&1; then runuser -u "$owner" -- sh -c "$1"
          else su -s /bin/sh "$owner" -c "$1"; fi
        }
        case "$mode" in
        get) as_owner "env $x $get" ;;
        set)
          # The tool forks to keep owning the selection; its output must not
          # hold the exec session open, so stderr goes through a file.
          err=$(mktemp)
          printf %s "$ARCBOX_CLIPBOARD" | base64 -d | as_owner "env $x $put" >/dev/null 2>"$err"
          rc=$?
          cat "$err" >&2
          rm -f "$err"
          exit $rc ;;
        *) exit 2 ;;
        esac
        """#

    nonisolated struct Failure: Error, Equatable, Sendable {
        let message: String
    }

    static func request(machine: String, mode: String, text: String? = nil) -> Arcbox_V1_MachineExecRequest {
        var request = Arcbox_V1_MachineExecRequest()
        request.id = machine
        request.cmd = ["/bin/sh", "-c", script, "arcbox-clipboard", mode]
        if let text {
            request.env["ARCBOX_CLIPBOARD"] = Data(text.utf8).base64EncodedString()
        }
        return request
    }

    /// Checks text before sending it; `nil` when it can be sent.
    static func validate(_ text: String?) -> Failure? {
        guard let text, !text.isEmpty else {
            return Failure(message: "The Mac clipboard has no text to send.")
        }
        guard text.utf8.count <= maxBytes else {
            return Failure(message: "The Mac clipboard text is larger than 64 KB.")
        }
        return nil
    }

    /// Turns a finished `get` or `set` into the text read, or a message.
    static func result(exitCode: Int32, stdout: Data, stderr: Data) -> Result<String, Failure> {
        let errorText = String(decoding: stderr, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        switch exitCode {
        case 0:
            return .success(String(decoding: stdout, as: UTF8.self))
        case noDisplayExit:
            return .failure(Failure(message: "No graphical desktop is running in this machine (no X display found)."))
        case noToolExit:
            return .failure(
                Failure(
                    message: "Install xclip in the machine to share its clipboard, for example: sudo apt install xclip")
            )
        default:
            if errorText.contains("not available") {
                return .failure(Failure(message: "The machine's clipboard has no text."))
            }
            let detail = errorText.isEmpty ? "exit code \(exitCode)" : errorText
            return .failure(Failure(message: "The machine's clipboard could not be reached: \(detail)"))
        }
    }

    /// Reads the machine's clipboard text.
    static func read(machine: String, client: ArcBoxClient) async -> Result<String, Failure> {
        await run(request(machine: machine, mode: "get"), client: client)
    }

    /// Replaces the machine's clipboard with `text`.
    static func write(_ text: String, machine: String, client: ArcBoxClient) async -> Result<Void, Failure> {
        if let failure = validate(text) {
            return .failure(failure)
        }
        return await run(request(machine: machine, mode: "set", text: text), client: client).map { _ in () }
    }

    private static func run(
        _ request: Arcbox_V1_MachineExecRequest, client: ArcBoxClient
    ) async -> Result<String, Failure> {
        do {
            let output = try await MachineExec.run(request, client: client)
            return result(exitCode: output.exitCode, stdout: output.stdout, stderr: output.stderr)
        } catch {
            return .failure(Failure(message: ArcBoxClient.userMessage(for: error)))
        }
    }
}

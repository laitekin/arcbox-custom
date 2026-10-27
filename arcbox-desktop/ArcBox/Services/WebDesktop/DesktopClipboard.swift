import AppKit
import ArcBoxClient
import CryptoKit
import Foundation
import GRPCCore

/// Binary-safe clipboard transport over the guest agent.
enum DesktopClipboard {
    static let maxBytes = 16 * 1024 * 1024

    nonisolated struct Payload: Equatable, Sendable {
        let mime: String
        let data: Data
        var fingerprint: String { mime + ":" + SHA256.hash(data: data).description }
    }

    nonisolated struct Failure: LocalizedError, Sendable {
        let message: String
        var errorDescription: String? { message }
    }

    static func readMac(_ board: NSPasteboard = .general) throws -> Payload? {
        if let png = board.data(forType: .png) {
            return try validate(Payload(mime: "image/png", data: png))
        }
        if let tiff = board.data(forType: .tiff), let bitmap = NSBitmapImageRep(data: tiff),
            let png = bitmap.representation(using: .png, properties: [:])
        {
            return try validate(Payload(mime: "image/png", data: png))
        }
        if let text = board.string(forType: .string) {
            return try validate(Payload(mime: "text/plain;charset=utf-8", data: Data(text.utf8)))
        }
        return nil
    }

    static func writeMac(_ payload: Payload, board: NSPasteboard = .general) throws {
        let payload = try validate(payload)
        board.clearContents()
        if payload.mime == "image/png" {
            board.setData(payload.data, forType: .png)
        } else {
            board.setString(String(decoding: payload.data, as: UTF8.self), forType: .string)
        }
    }

    static func validate(_ payload: Payload) throws -> Payload {
        guard ["image/png", "text/plain;charset=utf-8"].contains(payload.mime), payload.data.count <= maxBytes else {
            throw Failure(message: "Clipboard supports text and PNG images up to 16 MB.")
        }
        if payload.mime == "image/png" {
            guard payload.data.starts(with: [137, 80, 78, 71, 13, 10, 26, 10]),
                NSBitmapImageRep(data: payload.data) != nil
            else { throw Failure(message: "The clipboard image is not a valid PNG.") }
        } else if String(data: payload.data, encoding: .utf8) == nil {
            throw Failure(message: "Clipboard text is not valid UTF-8.")
        }
        return payload
    }

    /// Bind to a specific X display instead of silently selecting another session.
    static func request(
        machine: String, display: UInt16, mode: String, mime: String = ""
    )
        -> Arcbox_V1_MachineExecRequest
    {
        var request = Arcbox_V1_MachineExecRequest()
        request.id = machine
        request.cmd = ["/bin/sh", "-c", script, "arcbox-desktop-clipboard", mode, String(display), mime]
        request.attachStdin = mode == "set"
        return request
    }

    private static func options(seconds: Int64) -> CallOptions {
        var options = CallOptions.defaults
        options.timeout = .seconds(seconds)
        return options
    }

    static func read(machine: String, display: UInt16, client: ArcBoxClient) async throws -> Payload? {
        let output = try await MachineExec.run(
            request(machine: machine, display: display, mode: "get"), client: client,
            options: options(seconds: 8), maxOutputBytes: maxBytes + 1024
        )
        if output.exitCode == 72 { return nil }
        try check(output)
        guard let separator = output.stdout.firstIndex(of: 10) else {
            throw Failure(message: "Invalid clipboard response from the machine.")
        }
        let mime = String(decoding: output.stdout[..<separator], as: UTF8.self)
        return try validate(Payload(mime: mime, data: Data(output.stdout[(separator + 1)...])))
    }

    static func write(_ payload: Payload, machine: String, display: UInt16, client: ArcBoxClient) async throws {
        let payload = try validate(payload)
        let request = request(machine: machine, display: display, mode: "set", mime: payload.mime)
        let output = try await client.machines.execSession(
            options: options(seconds: 12),
            requestProducer: { writer in
                var first = Arcbox_V1_MachineExecInput()
                first.init_p = request
                try await writer.write(first)
                for offset in stride(from: 0, to: payload.data.count, by: 48 * 1024) {
                    var input = Arcbox_V1_MachineExecInput()
                    input.stdin = payload.data.subdata(in: offset..<min(offset + 48 * 1024, payload.data.count))
                    try await writer.write(input)
                }
                var eof = Arcbox_V1_MachineExecInput()
                eof.stdin = Data()
                try await writer.write(eof)
            },
            onResponse: { response in
                var stderr = Data()
                var exitCode: Int32 = -1
                for try await output in response.messages {
                    if stderr.count + output.data.count > 64 * 1024 {
                        throw Failure(message: "Clipboard helper produced too much output.")
                    }
                    if output.stream == "stderr" { stderr.append(output.data) }
                    if output.done { exitCode = output.exitCode }
                }
                return MachineExec.Output(exitCode: exitCode, stdout: Data(), stderr: stderr)
            }
        )
        try check(output)
    }

    /// Whether the focused window in the guest is a terminal, which takes Ctrl+Shift+C/V
    /// rather than Ctrl+C/V. `nil` when the guest has neither xdotool nor xprop.
    static func focusIsTerminal(machine: String, display: UInt16, client: ArcBoxClient) async throws -> Bool? {
        let output = try await MachineExec.run(
            request(machine: machine, display: display, mode: "focus"), client: client,
            options: options(seconds: 4), maxOutputBytes: 1024
        )
        try check(output)
        switch String(decoding: output.stdout, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines) {
        case "terminal": return true
        case "desktop": return false
        default: return nil
        }
    }

    private static func check(_ output: MachineExec.Output) throws {
        guard output.exitCode != 0 else { return }
        let detail = String(decoding: output.stderr, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        let message: String
        switch output.exitCode {
        case 70: message = "No X11 desktop on this display. Check the X display setting."
        case 71: message = "Install xclip in the machine (sudo apt install xclip) for text and image sharing."
        case 73: message = "The machine clipboard exceeds the 16 MB limit."
        default: message = detail.isEmpty ? "Clipboard helper exited with code \(output.exitCode)." : detail
        }
        throw Failure(message: message)
    }

    static let script = #"""
        mode="$1"; display="$2"; mime="$3"
        case "$display" in ''|*[!0-9]*) exit 70;; esac
        socket="/tmp/.X11-unix/X$display"
        [ -S "$socket" ] || exit 70
        command -v xclip >/dev/null 2>&1 || exit 71
        owner=$(stat -c %U "$socket") || exit 70
        home=$(getent passwd "$owner" | cut -d: -f6)
        [ -n "$home" ] || exit 70
        as_owner() {
          if [ "$(id -un)" = "$owner" ]; then "$@"
          elif command -v runuser >/dev/null 2>&1; then runuser -u "$owner" -- "$@"
          else su -s /bin/sh "$owner" -c 'exec "$@"' -- arcbox-clipboard "$@"; fi
        }
        x() {
          if [ -f "$home/.Xauthority" ]; then
            as_owner env "DISPLAY=:$display" "HOME=$home" "XAUTHORITY=$home/.Xauthority" "$@"
          else
            as_owner env "DISPLAY=:$display" "HOME=$home" "$@"
          fi
        }
        clip() { x xclip -selection clipboard "$@"; }
        if [ "$mode" = focus ]; then
          # Prints terminal, desktop or unknown for the focused window's WM_CLASS.
          window= class=
          if command -v xdotool >/dev/null 2>&1; then
            window=$(x xdotool getactivewindow 2>/dev/null) || window=$(x xdotool getwindowfocus 2>/dev/null) || window=
          fi
          if command -v xprop >/dev/null 2>&1; then
            [ -n "$window" ] || window=$(x xprop -root _NET_ACTIVE_WINDOW 2>/dev/null | sed -n 's/.*# \(0x[0-9a-fA-F]*\).*/\1/p')
            [ -z "$window" ] || class=$(x xprop -id "$window" WM_CLASS 2>/dev/null | sed -n 's/^WM_CLASS[^=]*= //p')
          elif [ -n "$window" ]; then
            class=$(x xdotool getwindowclassname "$window" 2>/dev/null) || class=
          fi
          class=$(printf '%s' "$class" | tr 'A-Z' 'a-z')
          case "$class" in
          '') echo unknown ;;
          *term*|*konsole*|*kitty*|*alacritty*|*tilix*|*rxvt*|*foot*|*guake*|*tilda*|*sakura*|*ptyxis*|*kgx*|*console*|st|st-*|*'"st"'*|*'"st-'*) echo terminal ;;
          *) echo desktop ;;
          esac
          exit 0
        fi
        tmp=$(mktemp -d) || exit 1
        trap 'rm -rf "$tmp"' EXIT HUP INT TERM
        case "$mode" in
        get)
          targets=$(clip -o -t TARGETS 2>/dev/null) || exit 72
          if printf '%s\n' "$targets" | grep -qx 'image/png'; then target=image/png; mime=image/png
          elif printf '%s\n' "$targets" | grep -qx UTF8_STRING; then target=UTF8_STRING; mime='text/plain;charset=utf-8'
          elif printf '%s\n' "$targets" | grep -qx 'text/plain;charset=utf-8'; then target='text/plain;charset=utf-8'; mime="$target"
          else exit 72; fi
          (ulimit -f 32770; clip -o -t "$target" >"$tmp/data") || exit 73
          [ "$(wc -c <"$tmp/data")" -le 16777216 ] || exit 73
          printf '%s\n' "$mime"; cat "$tmp/data" ;;
        set)
          case "$mime" in image/png) target=image/png;; 'text/plain;charset=utf-8') target=UTF8_STRING;; *) exit 2;; esac
          (ulimit -f 32770; cat >"$tmp/data") || exit 73
          [ "$(wc -c <"$tmp/data")" -le 16777216 ] || exit 73
          # Detach the selection owner's output from the exec stream.
          clip -i -t "$target" <"$tmp/data" >/dev/null 2>"$tmp/error"
          result=$?; cat "$tmp/error" >&2; exit "$result" ;;
        *) exit 2;;
        esac
        """#
}

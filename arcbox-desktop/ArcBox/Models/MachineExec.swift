import ArcBoxClient
import Foundation
import GRPCCore

/// Runs a command in a machine through its agent and collects what it
/// printed, for short helpers (the clipboard bridge, the isolation check).
enum MachineExec {
    nonisolated struct Output: Sendable {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
    }

    static func run(
        _ request: Arcbox_V1_MachineExecRequest,
        client: ArcBoxClient,
        options: CallOptions = ArcBoxClient.defaultCallOptions,
        maxOutputBytes: Int = 16 * 1024 * 1024
    ) async throws -> Output {
        try await client.machines.exec(request, options: options) { response in
            var stdout = Data()
            var stderr = Data()
            var exitCode: Int32 = -1
            for try await output in response.messages {
                guard stdout.count + stderr.count + output.data.count <= maxOutputBytes else {
                    throw RPCError(code: .resourceExhausted, message: "Machine command output exceeded its limit.")
                }
                if output.stream == "stderr" {
                    stderr.append(output.data)
                } else {
                    stdout.append(output.data)
                }
                if output.done {
                    exitCode = output.exitCode
                }
            }
            return Output(exitCode: exitCode, stdout: stdout, stderr: stderr)
        }
    }
}

import ArcBoxClient
import XCTest

@testable import ArcBox

@MainActor
final class MachineResourcesTests: XCTestCase {

    // MARK: - Draft

    func testDraftStartsCleanAndSendsOnlyChangedFields() {
        var draft = MachineResourceDraft(cpus: 4, memoryGiB: 4)
        XCTAssertFalse(draft.isDirty)

        draft.memoryGiB = 8
        XCTAssertTrue(draft.isDirty)
        let request = draft.request(machine: "dev")
        XCTAssertEqual(request.id, "dev")
        XCTAssertEqual(request.cpus, 0, "an unchanged field is sent as 0, which keeps it")
        XCTAssertEqual(request.memory, 8 << 30)

        draft.cpus = 6
        XCTAssertEqual(draft.request(machine: "dev").cpus, 6)

        draft.cpus = 4
        draft.memoryGiB = 4
        XCTAssertFalse(draft.isDirty)
    }

    func testLimitsRanges() {
        let limits = MachineResourceLimits(hostCPUs: 10, hostMemoryGiB: 32)
        XCTAssertEqual(limits.cpuRange, 1...10)
        XCTAssertEqual(limits.memoryRange, 1...32)

        let degenerate = MachineResourceLimits(hostCPUs: 0, hostMemoryGiB: 0)
        XCTAssertEqual(degenerate.cpuRange, 1...1)
        XCTAssertEqual(degenerate.memoryRange, 1...1)
    }

    // MARK: - Guest usage

    private func systemInfo(
        total: UInt64, available: UInt64, cpus: UInt32, load: [Double]
    ) -> Arcbox_V1_MachineSystemInfo {
        var info = Arcbox_V1_MachineSystemInfo()
        info.totalMemory = total
        info.availableMemory = available
        info.cpuCount = cpus
        info.loadAverage = load
        return info
    }

    func testUsageFromSystemInfo() {
        let usage = GuestResourceUsage(
            from: systemInfo(total: 4 << 30, available: 3 << 30, cpus: 4, load: [0.5, 0.2, 0.1]))
        XCTAssertEqual(usage.usedMemory, 1 << 30)
        XCTAssertEqual(usage.memoryFraction, 0.25, accuracy: 0.0001)
        XCTAssertEqual(usage.load1, 0.5)
        XCTAssertEqual(usage.cpuDisplay, "load 0.50 on 4 CPUs")
        XCTAssertFalse(usage.bootedWithDifferentCPUs(than: 4))
        XCTAssertTrue(usage.bootedWithDifferentCPUs(than: 6))
    }

    func testUsageToleratesOddReports() {
        let usage = GuestResourceUsage(from: systemInfo(total: 0, available: 5, cpus: 0, load: []))
        XCTAssertEqual(usage.usedMemory, 0)
        XCTAssertEqual(usage.memoryFraction, 0)
        XCTAssertNil(usage.cpuDisplay)
        XCTAssertFalse(usage.bootedWithDifferentCPUs(than: 4), "an unknown CPU count never asks for a restart")
    }

    // MARK: - Clipboard

    func testClipboardRequestCarriesModeAndBase64Text() {
        let request = MachineClipboard.request(machine: "dev", mode: "set", text: "héllo")
        XCTAssertEqual(request.id, "dev")
        XCTAssertEqual(request.cmd.first, "/bin/sh")
        XCTAssertEqual(request.cmd.last, "set")
        XCTAssertEqual(request.cmd[3], "arcbox-clipboard", "$0 for the script, so $1 is the mode")
        XCTAssertEqual(
            request.env["ARCBOX_CLIPBOARD"].flatMap { Data(base64Encoded: $0) }.map {
                String(decoding: $0, as: UTF8.self)
            },
            "héllo"
        )
        XCTAssertNil(MachineClipboard.request(machine: "dev", mode: "get").env["ARCBOX_CLIPBOARD"])
    }

    func testClipboardValidation() {
        XCTAssertNotNil(MachineClipboard.validate(nil))
        XCTAssertNotNil(MachineClipboard.validate(""))
        XCTAssertNil(MachineClipboard.validate("text"))
        XCTAssertNil(MachineClipboard.validate(String(repeating: "a", count: MachineClipboard.maxBytes)))
        XCTAssertNotNil(MachineClipboard.validate(String(repeating: "a", count: MachineClipboard.maxBytes + 1)))
    }

    func testClipboardResultMapping() {
        XCTAssertEqual(
            MachineClipboard.result(exitCode: 0, stdout: Data("hi".utf8), stderr: Data()),
            .success("hi")
        )
        guard
            case .failure(let noDisplay) = MachineClipboard.result(exitCode: 70, stdout: Data(), stderr: Data()),
            case .failure(let noTool) = MachineClipboard.result(exitCode: 71, stdout: Data(), stderr: Data()),
            case .failure(let empty) = MachineClipboard.result(
                exitCode: 1, stdout: Data(), stderr: Data("Error: target STRING not available\n".utf8)),
            case .failure(let other) = MachineClipboard.result(
                exitCode: 1, stdout: Data(), stderr: Data("Error: Can't open display: :1\n".utf8))
        else {
            return XCTFail("non-zero exits must fail")
        }
        XCTAssertTrue(noDisplay.message.contains("X display"))
        XCTAssertTrue(noTool.message.contains("xclip"))
        XCTAssertEqual(empty.message, "The machine's clipboard has no text.")
        XCTAssertTrue(other.message.hasSuffix("Error: Can't open display: :1"))
    }
}

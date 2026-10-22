import ArcBoxClient
import XCTest

@testable import ArcBox

@MainActor
final class ProxyChoiceTests: XCTestCase {

    // MARK: - Default choice

    private func customDefault() -> EgressProxySettings {
        var settings = EgressProxySettings()
        settings.mode = .custom
        settings.host = "proxy.example.com"
        settings.port = "1080"
        settings.exclude = ["10.0.0.0/8"]
        settings.udp = .block
        return settings
    }

    func testDefaultChoiceNamesModeOrProfile() {
        var settings = EgressProxySettings()
        XCTAssertEqual(ProxyChoice.defaultChoice(of: settings), "system")
        settings.mode = .direct
        XCTAssertEqual(ProxyChoice.defaultChoice(of: settings), "direct")
        settings = customDefault()
        XCTAssertEqual(ProxyChoice.defaultChoice(of: settings), "custom")
        settings.profileID = "6F1C1F7E-0B5E-4C61-9A50-6D2D3A2F9A10"
        XCTAssertEqual(ProxyChoice.defaultChoice(of: settings), settings.profileID)
    }

    func testSwitchingDefaultKeepsTrafficPolicies() throws {
        let current = customDefault()

        let direct = try XCTUnwrap(ProxyChoice.settings(forDefault: "direct", current: current))
        XCTAssertEqual(direct.mode, .direct)
        XCTAssertEqual(direct.profileID, "")
        XCTAssertEqual(direct.exclude, ["10.0.0.0/8"])
        XCTAssertEqual(direct.udp, .block)

        let system = try XCTUnwrap(ProxyChoice.settings(forDefault: "system", current: current))
        XCTAssertEqual(system.mode, .system)

        let saved = try XCTUnwrap(ProxyChoice.settings(forDefault: "profile-1", current: direct))
        XCTAssertEqual(saved.mode, .custom)
        XCTAssertEqual(saved.profileID, "profile-1")
        XCTAssertEqual(saved.exclude, ["10.0.0.0/8"])

        XCTAssertNil(
            ProxyChoice.settings(forDefault: "custom", current: current), "only Settings edits a one-off proxy")
        XCTAssertNil(ProxyChoice.settings(forDefault: "", current: current))
    }

    // MARK: - Test summary

    private func step(
        _ kind: Arcbox_V1_EgressProxyTestStepKind,
        ok: Bool,
        latency: UInt64 = 0,
        failure: Arcbox_V1_EgressProxyTestFailure = .unspecified,
        skipped: Bool = false
    ) -> EgressProxyTestStep {
        var proto = Arcbox_V1_EgressProxyTestStep()
        proto.step = kind
        proto.ok = ok
        proto.latencyMs = latency
        proto.failure = failure
        proto.skipped = skipped
        return EgressProxyTestStep(proto: proto)
    }

    func testSummaryStates() {
        XCTAssertEqual(ProxyTestSummary(steps: nil, isRunning: false), .untested)
        XCTAssertEqual(ProxyTestSummary(steps: [], isRunning: false), .untested)
        XCTAssertEqual(ProxyTestSummary(steps: nil, isRunning: true), .running)

        let passed = ProxyTestSummary(
            steps: [step(.reach, ok: true, latency: 12), step(.tunnel, ok: true, latency: 85)],
            isRunning: false
        )
        XCTAssertEqual(passed, .passed(latencyMilliseconds: 85, udpFailed: false))
        XCTAssertEqual(passed.label, "Working · 85 ms")
        XCTAssertFalse(passed.isFailure)
    }

    func testSummaryReportsFirstTCPFailure() {
        let summary = ProxyTestSummary(
            steps: [
                step(.reach, ok: true, latency: 10),
                step(.authenticate, ok: false, failure: .authRejected),
                step(.tunnel, ok: false, skipped: true),
            ],
            isRunning: false
        )
        XCTAssertEqual(
            summary,
            .failed(step: "Authenticate", reason: "The user name or password was rejected")
        )
        XCTAssertTrue(summary.isFailure)
    }

    func testUDPOnlyFailureStillWorks() {
        let summary = ProxyTestSummary(
            steps: [
                step(.reach, ok: true, latency: 10),
                step(.tunnel, ok: true, latency: 40),
                step(.udp, ok: false, failure: .udpUnsupported),
            ],
            isRunning: false
        )
        XCTAssertEqual(summary, .passed(latencyMilliseconds: 40, udpFailed: true))
        XCTAssertEqual(summary.label, "Working · 40 ms · no UDP relay")
        XCTAssertFalse(summary.isFailure)
    }

    // MARK: - Editor paste import

    func testPastedURLFillsFieldsAndPartialURLIsLeftAlone() {
        let editor = ProxyProfileEditorModel(profile: nil)

        editor.urlInput = "not a url"
        editor.importIfComplete()
        XCTAssertEqual(editor.urlInput, "not a url", "an incomplete paste stays for the user to fix")
        XCTAssertNil(editor.error)
        XCTAssertFalse(editor.hasInput)

        editor.urlInput = "socks5h://alice:s3cret@proxy.example.com:1081"
        editor.importIfComplete()
        XCTAssertEqual(editor.urlInput, "")
        XCTAssertEqual(editor.draft.host, "proxy.example.com")
        XCTAssertEqual(editor.draft.port, "1081")
        XCTAssertEqual(editor.draft.username, "alice")
        XCTAssertEqual(editor.password, "s3cret")
        XCTAssertTrue(editor.draft.remoteDNS)
        XCTAssertEqual(editor.name, "proxy.example.com:1081")
        XCTAssertEqual(editor.lastImported, "proxy.example.com:1081")
        XCTAssertNil(editor.fieldProblem)
    }

    func testEditorFlagsMissingPassword() {
        let editor = ProxyProfileEditorModel(profile: nil)
        editor.draft.host = "proxy.example.com"
        editor.draft.port = "1080"
        XCTAssertNil(editor.fieldProblem)
        editor.draft.username = "alice"
        XCTAssertEqual(editor.fieldProblem, "Enter the proxy password.")
        editor.password = "pw"
        XCTAssertNil(editor.fieldProblem)
        editor.draft.port = "99999"
        XCTAssertNotNil(editor.fieldProblem)
    }
}

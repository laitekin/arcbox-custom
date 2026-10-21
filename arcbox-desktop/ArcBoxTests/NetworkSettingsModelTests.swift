import ArcBoxClient
import XCTest

@testable import ArcBox

@MainActor
final class NetworkSettingsModelTests: XCTestCase {
    func testAFreshModelHasNothingToSave() {
        let model = NetworkSettingsModel()
        XCTAssertFalse(model.isDirty)
        XCTAssertFalse(model.needsPassword)
        XCTAssertNil(model.validationError)
    }

    func testEditingTheDraftOrTypingAPasswordMakesItDirty() {
        let model = NetworkSettingsModel()
        model.draft.mode = .direct
        XCTAssertTrue(model.isDirty)
        model.draft.mode = .system
        XCTAssertFalse(model.isDirty)
        model.password = "secret"
        XCTAssertTrue(model.isDirty)
    }

    func testExclusionsCompareAsParsedLines() {
        let model = NetworkSettingsModel()
        model.excludeText = "\n  ,\n"
        XCTAssertFalse(model.isDirty, "blank lines and commas are not exclusions")
        model.excludeText = "internal.corp, 10.0.0.0/8"
        XCTAssertTrue(model.isDirty)
    }

    func testANewProxyUserNeedsAPassword() {
        let model = NetworkSettingsModel()
        model.draft.mode = .custom
        model.draft.host = "proxy.corp"
        model.draft.port = "1080"
        XCTAssertFalse(model.needsPassword, "no user, no password")
        XCTAssertNil(model.validationError)

        model.draft.username = "alice"
        XCTAssertTrue(model.needsPassword)
        XCTAssertEqual(model.validationError, "Enter the password for alice.")

        model.password = "secret"
        XCTAssertFalse(model.needsPassword)
        XCTAssertNil(model.validationError)
    }

    func testDraftErrorsComeBeforeThePasswordPrompt() {
        let model = NetworkSettingsModel()
        model.draft.mode = .custom
        model.draft.username = "alice"
        XCTAssertEqual(model.validationError, "Enter the proxy server's address.")
        model.draft.host = "proxy.corp"
        model.draft.port = "99999"
        XCTAssertEqual(model.validationError, "Enter a port between 1 and 65535.")
    }

    func testRevertDropsTheDraft() {
        let model = NetworkSettingsModel()
        model.draft.mode = .direct
        model.password = "secret"
        model.excludeText = "a.example"
        model.revert()
        XCTAssertFalse(model.isDirty)
        XCTAssertEqual(model.password, "")
        XCTAssertEqual(model.excludeText, "")
    }
}

import ArcBoxClient
import XCTest

@testable import ArcBox

@MainActor
final class MachineIsolationTests: XCTestCase {

    private func profile(_ id: String) -> SavedProxyProfile {
        var proto = Arcbox_V1_ProxyProfile()
        proto.id = id
        proto.name = id
        return SavedProxyProfile(proto: proto)
    }

    func testIsolatedMachinesMayOnlyUseSavedProxies() {
        let profiles = [profile("p1"), profile("p2")]
        XCTAssertTrue(MachineIsolation.allowsProxyChoice("", isolated: false, profiles: profiles))
        XCTAssertTrue(MachineIsolation.allowsProxyChoice("direct", isolated: false, profiles: profiles))
        XCTAssertFalse(MachineIsolation.allowsProxyChoice("", isolated: true, profiles: profiles))
        XCTAssertFalse(MachineIsolation.allowsProxyChoice("direct", isolated: true, profiles: profiles))
        XCTAssertFalse(MachineIsolation.allowsProxyChoice("gone", isolated: true, profiles: profiles))
        XCTAssertTrue(MachineIsolation.allowsProxyChoice("p2", isolated: true, profiles: profiles))
    }

    func testIsolatingPicksASavedProxy() {
        let profiles = [profile("p1"), profile("p2")]
        XCTAssertEqual(MachineIsolation.proxyChoice(afterIsolating: "", profiles: profiles), "p1")
        XCTAssertEqual(MachineIsolation.proxyChoice(afterIsolating: "direct", profiles: profiles), "p1")
        XCTAssertEqual(MachineIsolation.proxyChoice(afterIsolating: "p2", profiles: profiles), "p2")
        XCTAssertEqual(MachineIsolation.proxyChoice(afterIsolating: "direct", profiles: []), "")
    }

    func testCreateIsBlockedUntilASavedProxyIsChosen() {
        XCTAssertNil(MachineIsolation.createBlocker(isolated: false, proxyChoice: "", profiles: []))
        XCTAssertEqual(
            MachineIsolation.createBlocker(isolated: true, proxyChoice: "", profiles: []),
            "An isolated machine needs a saved proxy. Import one on the Proxies page first."
        )
        XCTAssertEqual(
            MachineIsolation.createBlocker(isolated: true, proxyChoice: "direct", profiles: [profile("p1")]),
            "Choose a saved proxy for the isolated machine."
        )
        XCTAssertNil(MachineIsolation.createBlocker(isolated: true, proxyChoice: "p1", profiles: [profile("p1")]))
    }

    func testChecklistIsHonestAboutWhatIsCovered() {
        let open = MachineIsolation.items(isolated: false, mountCount: 0)
        XCTAssertTrue(open.allSatisfy { $0.status == .notCovered })

        let sealed = MachineIsolation.items(isolated: true, mountCount: 0)
        XCTAssertEqual(sealed.map(\.status), [.covered, .covered, .notCovered])
        XCTAssertTrue(sealed.last!.text.hasPrefix("Network is not isolated yet"))

        let withMounts = MachineIsolation.items(isolated: true, mountCount: 2)
        XCTAssertEqual(withMounts.map(\.status), [.covered, .covered, .warning, .notCovered])
        XCTAssertEqual(withMounts[2].text, "2 folders you added are still shared")
        XCTAssertEqual(
            MachineIsolation.items(isolated: true, mountCount: 1)[2].text, "1 folder you added is still shared")
    }

    func testViewModelCarriesIsolation() {
        var summary = Arcbox_V1_MachineSummary()
        summary.id = "box"
        summary.name = "box"
        summary.isolated = true
        var machine = MachineViewModel(from: summary)
        XCTAssertTrue(machine.isolated)

        var info = Arcbox_V1_MachineInfo()
        info.isolated = false
        machine.applyDetails(from: info)
        XCTAssertFalse(machine.isolated, "Inspect is the fresher answer")
    }
}

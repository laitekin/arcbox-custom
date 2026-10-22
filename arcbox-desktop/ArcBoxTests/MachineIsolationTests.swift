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

    func testChecklistCoversFilesNetworkAndIdentity() {
        let open = MachineIsolation.items(isolated: false, mountCount: 0)
        XCTAssertTrue(open.allSatisfy { $0.status == .notCovered })
        XCTAssertTrue(open.contains { $0.text.contains("bypassing its proxy") })

        let sealed = MachineIsolation.items(isolated: true, mountCount: 0)
        XCTAssertEqual(sealed.count, 5)
        XCTAssertTrue(sealed.allSatisfy { $0.status == .covered })

        let withMounts = MachineIsolation.items(isolated: true, mountCount: 2)
        XCTAssertEqual(withMounts.last?.status, .warning)
        XCTAssertEqual(withMounts.last?.text, "2 folders you added are still shared")
        XCTAssertEqual(
            MachineIsolation.items(isolated: true, mountCount: 1).last?.text, "1 folder you added is still shared")
    }

    func testIsolationCheckOutputIsParsed() {
        let output = """
            files pass the Mac's /Users and /private cannot be mounted
            nics fail 2 network interfaces; a second one could bypass the proxy
            exit info the internet sees 203.0.113.9, which should be your proxy's address
            garbage
            ping maybe something
            """
        let results = IsolationCheck.parse(output)
        XCTAssertEqual(results.map(\.key), ["files", "nics", "exit"])
        XCTAssertEqual(results[1].status, .fail)
        XCTAssertEqual(results[1].title, "Network interfaces")
        XCTAssertEqual(results[2].detail, "the internet sees 203.0.113.9, which should be your proxy's address")
        XCTAssertFalse(IsolationCheck.passed(results))
        XCTAssertTrue(IsolationCheck.passed([results[0], results[2]]))
        XCTAssertFalse(IsolationCheck.passed([]), "no results is not a pass")
        XCTAssertEqual(IsolationCheck.request(machine: "box").cmd.prefix(2), ["/bin/sh", "-c"])
    }

    func testPortForwardsRoundTripTheWire() {
        let vnc = MachinePortForwardViewModel(hostPort: 15901, guestPort: 5901)
        XCTAssertEqual(MachinePortForwardViewModel(from: vnc.proto), vnc)
        var udp = Arcbox_V1_MachinePortForward()
        udp.hostPort = 53
        udp.guestPort = 53
        udp.protocol = "UDP"
        XCTAssertEqual(MachinePortForwardViewModel(from: udp)?.udp, true)
        udp.hostPort = 70_000
        XCTAssertNil(MachinePortForwardViewModel(from: udp))
        XCTAssertNotEqual(vnc.id, MachinePortForwardViewModel(hostPort: 15901, guestPort: 5901, udp: true).id)
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
        info.timezone = "Asia/Tokyo"
        info.locale = "ja_JP.UTF-8"
        info.portForwards = [MachinePortForwardViewModel(hostPort: 2222, guestPort: 22).proto]
        machine.applyDetails(from: info)
        XCTAssertFalse(machine.isolated, "Inspect is the fresher answer")
        XCTAssertEqual(machine.timezone, "Asia/Tokyo")
        XCTAssertEqual(machine.portForwards, [MachinePortForwardViewModel(hostPort: 2222, guestPort: 22)])

        // A list refresh keeps what only Inspect reports.
        var refreshed = MachineViewModel(from: summary)
        refreshed.preserveDetailFrom(machine)
        XCTAssertEqual(refreshed.locale, "ja_JP.UTF-8")
        XCTAssertEqual(refreshed.portForwards.count, 1)
    }
}

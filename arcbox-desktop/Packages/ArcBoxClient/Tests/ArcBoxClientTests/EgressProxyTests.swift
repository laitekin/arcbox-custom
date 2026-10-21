import ArcBoxClient
import Foundation
import Testing

@Suite struct EgressProxySettingsTests {
    private func reported(_ url: String, mode: Arcbox_V1_EgressProxyMode = .custom) -> Arcbox_V1_EgressProxySettings {
        var settings = Arcbox_V1_EgressProxySettings()
        settings.mode = mode
        settings.url = url
        settings.hasPassword_p = true
        settings.exclude = [".corp.example"]
        settings.udp = .block
        settings.effective = [url]
        settings.credentialStore = "keychain"
        return settings
    }

    @Test func aReportedCustomProxySplitsIntoFields() {
        let settings = EgressProxySettings(proto: reported("socks5://ali%40ce@10.0.0.2:1080"))
        #expect(settings.mode == .custom)
        #expect(settings.proxyProtocol == .socks5)
        #expect(settings.host == "10.0.0.2")
        #expect(settings.port == "1080")
        #expect(settings.username == "ali@ce")
        #expect(settings.hasPassword)
        #expect(settings.udp == .block)
        #expect(settings.credentialStore == "keychain")
        // And builds the same URL back, user re-encoded.
        #expect(settings.url == "socks5://ali%40ce@10.0.0.2:1080")
    }

    @Test func ipv6HostsAndHttpProxiesRoundTrip() {
        let settings = EgressProxySettings(proto: reported("http://[::1]:3128"))
        #expect(settings.proxyProtocol == .http)
        #expect(settings.host == "::1")
        #expect(settings.username.isEmpty)
        #expect(settings.url == "http://[::1]:3128")
    }

    @Test func modesMapToTheWire() {
        #expect(EgressProxySettings(proto: reported("", mode: .none)).mode == .direct)
        #expect(EgressProxySettings(proto: reported("", mode: .unspecified)).mode == .system)

        var direct = EgressProxySettings()
        direct.mode = .direct
        #expect(direct.proto.mode == .none)
        #expect(direct.proto.url.isEmpty)
    }

    @Test func validationNamesTheProblem() {
        var settings = EgressProxySettings()
        #expect(settings.validationError == nil, "system needs no fields")
        settings.mode = .custom
        #expect(settings.validationError?.contains("address") == true)
        settings.host = "proxy.corp"
        settings.port = "0"
        #expect(settings.validationError?.contains("port") == true)
        settings.port = "1080"
        #expect(settings.validationError == nil)
        settings.proxyProtocol = .http
        settings.username = "a:b"
        #expect(settings.validationError?.contains("colon") == true)
    }

    @Test func passwordChangesMapToTheOptionalField() {
        var settings = EgressProxySettings()
        settings.mode = .custom
        settings.host = "h"
        settings.port = "1"
        settings.exclude = [" a.example ", ""]
        #expect(!settings.setRequest(password: .keep).hasPassword)
        #expect(settings.setRequest(password: .clear).password == "")
        #expect(settings.setRequest(password: .clear).hasPassword)
        #expect(settings.setRequest(password: .set("pw")).password == "pw")
        #expect(settings.setRequest(password: .keep).settings.exclude == ["a.example"])
    }

    @Test func aChangedUserIsANewAccount() {
        let saved = EgressProxySettings(proto: reported("socks5://alice@h:1"))
        var edited = saved
        #expect(edited.sameAccount(as: saved))
        edited.username = "bob"
        #expect(!edited.sameAccount(as: saved))
    }

    @Test func testStepsCarryTypedFailures() {
        var step = Arcbox_V1_EgressProxyTestStep()
        step.step = .authenticate
        step.failure = .authRejected
        step.message = "proxy authentication failed"
        let parsed = EgressProxyTestStep(proto: step)
        #expect(parsed.kind == .authenticate)
        #expect(!parsed.ok)
        #expect(parsed.failure?.contains("rejected") == true)
    }
}

@Suite struct MachineImageTests {
    @Test func namesFollowTheDaemonsRule() {
        for valid in ["my-ubuntu", "a", "a1.b_c"] {
            #expect(isValidMachineImageName(valid), "\(valid)")
        }
        for invalid in ["", ".hidden", "Upper", "a/b", "with space", String(repeating: "a", count: 64)] {
            #expect(!isValidMachineImageName(invalid), "\(invalid)")
        }
    }

    @Test func suggestedNamesFitTheNameRules() {
        #expect(suggestedMachineImageName(forFile: "ubuntu-rootfs.tar.gz") == "ubuntu-rootfs")
        #expect(suggestedMachineImageName(forFile: "Alpine 3.20.SQUASHFS") == "alpine-3.20")
        #expect(suggestedMachineImageName(forFile: "_my image_.tar") == "my-image")
        #expect(suggestedMachineImageName(forFile: "rootfs.tar.zst.bak") == "rootfs.tar.zst.bak")
        let long = suggestedMachineImageName(forFile: String(repeating: "a", count: 80) + ".tgz")
        #expect(long.count == 63)
        for name in ["ubuntu-rootfs.tar.gz", "Alpine 3.20.SQUASHFS", long + ".tar"] {
            #expect(isValidMachineImageName(suggestedMachineImageName(forFile: name)))
        }
    }

    @Test func importSourcesFillOneField() {
        let file = MachineImageImportSource.file(URL(fileURLWithPath: "/tmp/rootfs.tar.gz")).request(name: "x")
        #expect(file.path == "/tmp/rootfs.tar.gz")
        #expect(file.dockerImage.isEmpty)
        let docker = MachineImageImportSource.docker("ubuntu:24.04").request(name: "x")
        #expect(docker.dockerImage == "ubuntu:24.04")
        #expect(docker.path.isEmpty)
        #expect(!docker.addInit)
        let withInit = MachineImageImportSource.docker("alpine", addInit: true).request(name: "x")
        #expect(withInit.addInit)
    }

    @Test func theBuildingPhaseIsReported() {
        var event = Arcbox_V1_ImportMachineImageEvent()
        event.phase = .building
        #expect(MachineImageImportPhase(proto: event) == .building)
    }

    @Test func theDoneEventCarriesTheImage() {
        var event = Arcbox_V1_ImportMachineImageEvent()
        event.phase = .done
        event.image.name = "tiny"
        event.image.version = "local-1"
        event.image.imported = true
        event.warnings = ["w"]
        guard case .done(let image, let warnings) = MachineImageImportPhase(proto: event) else {
            Issue.record("not a done event")
            return
        }
        #expect(image.reference == "tiny@local-1")
        #expect(image.imported)
        #expect(warnings == ["w"])
        event.phase = .unspecified
        #expect(MachineImageImportPhase(proto: event) == nil)
    }
}

@Suite struct EgressProxyQuickToggleTests {
    private var custom: EgressProxySettings {
        var settings = EgressProxySettings()
        settings.mode = .custom
        settings.proxyProtocol = .http
        settings.host = "proxy.corp"
        settings.port = "3128"
        settings.username = "alice"
        settings.udp = .block
        return settings
    }

    @Test func offRemembersACustomProxyAndKeepsPolicies() throws {
        let remembered = try #require(EgressProxyQuickToggle.remembered(custom))
        #expect(remembered == "http://alice@proxy.corp:3128")
        let off = EgressProxyQuickToggle.off(custom)
        #expect(off.mode == .direct)
        #expect(off.udp == .block)
        #expect(EgressProxyQuickToggle.remembered(off) == nil)
    }

    @Test func onRestoresTheRememberedCustomProxy() {
        let off = EgressProxyQuickToggle.off(custom)
        let on = EgressProxyQuickToggle.on(off, remembered: EgressProxyQuickToggle.remembered(custom))
        #expect(on.sameAccount(as: custom))
        #expect(on.udp == .block)
        #expect(on.setRequest(password: .keep).hasPassword == false)
    }

    @Test func onFollowsMacOSWithoutAUsableMemory() {
        var direct = EgressProxySettings()
        direct.mode = .direct
        #expect(EgressProxyQuickToggle.on(direct, remembered: nil).mode == .system)
        #expect(EgressProxyQuickToggle.on(direct, remembered: "system").mode == .system)
        #expect(EgressProxyQuickToggle.on(direct, remembered: "socks5://:0").mode == .system)
        var system = EgressProxySettings()
        system.mode = .system
        #expect(EgressProxyQuickToggle.remembered(system) == "system")
    }
}

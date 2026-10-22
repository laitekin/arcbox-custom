import ArcBoxClient
import Foundation
import Testing

@Suite struct ProxyLibraryTests {
    @Test func importedNamesFollowTheEndpointUnlessTheUserChoseACustomName() throws {
        let first = try SocksURLImport.parse("socks5://127.0.0.1:7897").settings
        let firstName = SocksURLImport.suggestedName(for: first, currentName: "", previousSuggestion: nil)
        #expect(firstName == "127.0.0.1:7897")
        let second = try SocksURLImport.parse("socks5://198.51.100.8:20000").settings
        #expect(
            SocksURLImport.suggestedName(for: second, currentName: firstName, previousSuggestion: first.endpointLabel)
                == "198.51.100.8:20000")
        #expect(
            SocksURLImport.suggestedName(for: second, currentName: "My proxy", previousSuggestion: first.endpointLabel)
                == "My proxy")
    }

    @Test func selectorsShowTheActualEndpointAlongsideCustomNames() throws {
        var wire = Arcbox_V1_ProxyProfile()
        wire.name = "My proxy"
        wire.settings = try SocksURLImport.parse("socks5://[::1]:1080").settings.proto
        #expect(SavedProxyProfile(proto: wire).menuLabel == "My proxy ([::1]:1080)")
        wire.name = "[::1]:1080"
        #expect(SavedProxyProfile(proto: wire).menuLabel == "[::1]:1080")
    }
    @Test func importsEncodedCredentialsIPv6AndRemoteDNSWithoutEchoingThePassword() throws {
        let imported = try SocksURLImport.parse("socks5h://ali%40ce:p%40ss%3A%2F@[::1]:1081")
        #expect(imported.settings.host == "::1")
        #expect(imported.settings.port == "1081")
        #expect(imported.settings.username == "ali@ce")
        #expect(imported.password == "p@ss:/")
        #expect(imported.settings.remoteDNS)
        #expect(imported.settings.url == "socks5h://ali%40ce@[::1]:1081")
    }

    @Test func fillsTheStandardPortAndPreservesLocalDNSChoice() throws {
        let imported = try SocksURLImport.parse("  socks5://localhost  ")
        #expect(imported.settings.port == "1080")
        #expect(!imported.settings.remoteDNS)
        #expect(imported.password == nil)
    }

    @Test func rejectsUnsupportedURLsAndMalformedCredentialsWithoutLeakingInput() {
        for url in [
            "http://u:secret@host:1080", "socks5://u:secret@host:0", "socks5://u:secret@host:99999",
            "socks5://u:secret@host:1080/path", "socks5://u:bad%ZZ@host:1080", "socks5://:secret@host:1080",
        ] {
            do { _ = try SocksURLImport.parse(url); Issue.record("accepted an invalid proxy URL") } catch {
                #expect(!error.localizedDescription.contains("secret"));
                #expect(!error.localizedDescription.contains("bad%ZZ"))
            }
        }
    }

    @Test func savedProfileSurvivesQuickToggleWithoutCopyingCredentials() {
        var settings = EgressProxySettings()
        settings.mode = .custom
        settings.profileID = UUID().uuidString.lowercased()
        let remembered = EgressProxyQuickToggle.remembered(settings)
        let off = EgressProxyQuickToggle.off(settings)
        #expect(off.profileID.isEmpty)
        #expect(off.proto.mode == .none)
        let on = EgressProxyQuickToggle.on(off, remembered: remembered)
        #expect(on.profileID == settings.profileID)
        #expect(on.proto.profileID == settings.profileID)
        #expect(on.validationError == nil)
        var other = settings
        other.profileID = UUID().uuidString.lowercased()
        #expect(!other.sameAccount(as: settings))
    }
}

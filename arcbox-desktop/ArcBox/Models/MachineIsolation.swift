import ArcBoxClient
import Foundation

/// What isolating a machine from the Mac means for the app: which proxies it
/// may use and what the Info tab says it covers.
nonisolated enum MachineIsolation {
    /// One line of the isolation checklist.
    struct Item: Equatable, Identifiable {
        enum Status: Equatable { case covered, warning, notCovered }

        let status: Status
        let text: String
        var id: String { text }
    }

    /// An isolated machine never connects without a saved proxy: neither
    /// "follow the default" (which may be direct) nor "direct" is offered.
    static func allowsProxyChoice(_ choice: String, isolated: Bool, profiles: [SavedProxyProfile]) -> Bool {
        !isolated || profiles.contains { $0.id == choice }
    }

    /// The choice to switch to when a machine becomes isolated: the current
    /// one if it is allowed, else the first saved proxy, else none.
    static func proxyChoice(afterIsolating choice: String, profiles: [SavedProxyProfile]) -> String {
        allowsProxyChoice(choice, isolated: true, profiles: profiles) ? choice : profiles.first?.id ?? ""
    }

    /// Why an isolated machine cannot be created with `choice` yet.
    static func createBlocker(isolated: Bool, proxyChoice: String, profiles: [SavedProxyProfile]) -> String? {
        guard !allowsProxyChoice(proxyChoice, isolated: isolated, profiles: profiles) else { return nil }
        return profiles.isEmpty
            ? "An isolated machine needs a saved proxy. Import one on the Proxies page first."
            : "Choose a saved proxy for the isolated machine."
    }

    /// The checklist the Info tab shows for a machine.
    static func items(isolated: Bool, mountCount: Int) -> [Item] {
        guard isolated else {
            return [
                Item(status: .notCovered, text: "Can read and write your Mac's /Users and /private"),
                Item(status: .notCovered, text: "Can read ArcBox's data folder"),
            ]
        }
        var items = [
            Item(status: .covered, text: "No access to your Mac's /Users and /private"),
            Item(status: .covered, text: "Sees only its own folder, not ArcBox's data folder"),
        ]
        if mountCount > 0 {
            items.append(
                Item(
                    status: .warning,
                    text: mountCount == 1
                        ? "1 folder you added is still shared"
                        : "\(mountCount) folders you added are still shared"
                ))
        }
        items.append(
            Item(status: .notCovered, text: "Network is not isolated yet: it can reach your Mac and other machines"))
        return items
    }
}

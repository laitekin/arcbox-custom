import ArcBoxClient
import Foundation

/// Maps proxy pickers' choices to settings. A default choice is `system`,
/// `direct`, a saved proxy ID, or `custom` (a one-off proxy edited in
/// Settings); a machine's choice is empty (follow the default), `direct`,
/// or a saved proxy ID.
nonisolated enum ProxyChoice {
    static let system = EgressProxyMode.system.rawValue
    static let direct = EgressProxyMode.direct.rawValue
    static let custom = EgressProxyMode.custom.rawValue

    static func defaultChoice(of settings: EgressProxySettings) -> String {
        settings.profileID.isEmpty ? settings.mode.rawValue : settings.profileID
    }

    /// `current` switched to `choice`, keeping its bypass list and UDP/ICMP
    /// policies; nil for `custom`, which only Settings can describe.
    static func settings(forDefault choice: String, current: EgressProxySettings) -> EgressProxySettings? {
        var next = current
        switch choice {
        case system:
            next.mode = .system
            next.profileID = ""
        case direct:
            next.mode = .direct
            next.profileID = ""
        case custom, "":
            return nil
        default:
            next.mode = .custom
            next.profileID = choice
        }
        return next
    }
}

/// One line summarizing a proxy test, for a row that has no room for every
/// step.
nonisolated enum ProxyTestSummary: Equatable {
    case untested
    case running
    /// Every TCP step passed; the latency of reaching the target, and
    /// whether the UDP relay failed (TCP traffic still works then).
    case passed(latencyMilliseconds: UInt64, udpFailed: Bool)
    /// The first step that failed and why.
    case failed(step: String, reason: String)

    init(steps: [EgressProxyTestStep]?, isRunning: Bool) {
        if isRunning {
            self = .running
            return
        }
        guard let steps, !steps.isEmpty else {
            self = .untested
            return
        }
        let failures = steps.filter { !$0.ok && !$0.skipped }
        if let failed = failures.first(where: { $0.kind != .udp }) {
            let reason = failed.failure ?? failed.message
            self = .failed(step: failed.kind.title, reason: reason.isEmpty ? "Failed" : reason)
            return
        }
        let latency =
            steps.last(where: { $0.kind == .tunnel && $0.ok })?.latencyMilliseconds
            ?? steps.last(where: { $0.ok })?.latencyMilliseconds ?? 0
        self = .passed(latencyMilliseconds: latency, udpFailed: !failures.isEmpty)
    }

    var label: String {
        switch self {
        case .untested: "Not tested"
        case .running: "Testing…"
        case .passed(let latency, let udpFailed):
            ["Working", latency > 0 ? "\(latency) ms" : nil, udpFailed ? "no UDP relay" : nil]
                .compactMap { $0 }.joined(separator: " · ")
        case .failed(let step, let reason): "\(step) failed: \(reason)"
        }
    }

    var symbol: String {
        switch self {
        case .untested: "circle.dashed"
        case .running: "clock"
        case .passed: "checkmark.circle.fill"
        case .failed: "xmark.circle.fill"
        }
    }

    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

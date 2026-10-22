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
                Item(status: .notCovered, text: "Can reach your Mac, your network and other machines"),
                Item(status: .notCovered, text: "Can connect directly, bypassing its proxy"),
            ]
        }
        var items = [
            Item(status: .covered, text: "No access to your Mac's /Users and /private"),
            Item(status: .covered, text: "Sees only its own folder, not ArcBox's data folder"),
            Item(status: .covered, text: "No network path to your Mac, your network or other machines"),
            Item(status: .covered, text: "Internet and DNS only through its saved proxy; no ping"),
            Item(status: .covered, text: "Its own network card address and machine ID"),
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
        return items
    }
}

/// The isolation self-check: probes run inside the machine that try what
/// an isolated machine must not be able to do.
nonisolated enum IsolationCheck {
    struct Result: Equatable, Identifiable {
        enum Status: String, Equatable { case pass, fail, info }

        let key: String
        let status: Status
        let detail: String
        var id: String { key }

        var title: String {
            switch key {
            case "files": "Mac files"
            case "nics": "Network interfaces"
            case "host": "Your Mac"
            case "lan": "Local network"
            case "ping": "Ping"
            case "exit": "Internet exit"
            case "nic_address": "Network card address"
            default: key
            }
        }
    }

    /// Runs as root and prints one `key status detail` line per probe.
    /// Each probe uses only what a minimal image has (sh, mount, /sys) and
    /// says `info` rather than guess when a tool is missing.
    static let script = #"""
        say() { echo "$1 $2 $3"; }
        tcp() {
          if command -v nc >/dev/null 2>&1; then nc -z -w 3 "$1" "$2" >/dev/null 2>&1; return $?; fi
          if command -v bash >/dev/null 2>&1; then timeout 4 bash -c "exec 3<>/dev/tcp/$1/$2" >/dev/null 2>&1; return $?; fi
          return 2
        }
        d=$(mktemp -d)
        if mount -t virtiofs users "$d" 2>/dev/null; then umount "$d"; say files fail "the Mac's /Users can be mounted"
        elif mount -t virtiofs private "$d" 2>/dev/null; then umount "$d"; say files fail "the Mac's /private can be mounted"
        else say files pass "the Mac's /Users and /private cannot be mounted"; fi
        rmdir "$d"
        nics=$(ls /sys/class/net | grep -v '^lo$')
        n=$(echo "$nics" | grep -c .)
        if [ "$n" -le 1 ]; then say nics pass "one network interface"
        else say nics fail "$n network interfaces; a second one could bypass the proxy"; fi
        gw=$(ip route 2>/dev/null | awk '/^default/ {print $3; exit}'); [ -n "$gw" ] || gw=10.0.2.1
        result=pass; for port in 22 445 5900 8080; do
          tcp "$gw" "$port"; r=$?
          if [ "$r" -eq 0 ]; then result="fail port $port of the Mac answered"; break; fi
          if [ "$r" -eq 2 ]; then result=info; break; fi
        done
        case "$result" in
          pass) say host pass "the Mac's services do not answer" ;;
          info) say host info "no nc or bash to test with" ;;
          *) say host fail "${result#fail }" ;;
        esac
        result=pass; for target in 192.168.1.1 192.168.0.1 10.0.0.1 172.16.0.1; do
          tcp "$target" 80; r=$?
          if [ "$r" -eq 0 ]; then result="fail $target answered"; break; fi
          if [ "$r" -eq 2 ]; then result=info; break; fi
        done
        case "$result" in
          pass) say lan pass "local network addresses do not answer" ;;
          info) say lan info "no nc or bash to test with" ;;
          *) say lan fail "${result#fail }" ;;
        esac
        if command -v ping >/dev/null 2>&1; then
          if ping -c 1 -W 2 1.1.1.1 >/dev/null 2>&1; then say ping fail "ping reached the internet directly"; else say ping pass "ping is blocked"; fi
        else say ping info "no ping to test with"; fi
        ip=""
        if command -v curl >/dev/null 2>&1; then ip=$(curl -s -m 10 https://api.ipify.org)
        elif command -v wget >/dev/null 2>&1; then ip=$(wget -qO- -T 10 https://api.ipify.org); fi
        if [ -n "$ip" ]; then say exit info "the internet sees $ip, which should be your proxy's address"
        else say exit info "no answer from the internet (no proxy working, or no curl or wget)"; fi
        first=$(echo "$nics" | head -n 1)
        m=$(cat "/sys/class/net/$first/address" 2>/dev/null)
        case "$m" in
          "") say nic_address info "unknown" ;;
          ?[2367abefABEF]:*) say nic_address fail "$m is a generated (locally administered) address" ;;
          *) say nic_address pass "$m" ;;
        esac
        """#

    static func request(machine: String) -> Arcbox_V1_MachineExecRequest {
        var request = Arcbox_V1_MachineExecRequest()
        request.id = machine
        request.cmd = ["/bin/sh", "-c", script]
        return request
    }

    /// The probes' lines; unknown or malformed lines are skipped.
    static func parse(_ output: String) -> [Result] {
        output.split(whereSeparator: \.isNewline).compactMap { line in
            let parts = line.split(separator: " ", maxSplits: 2, omittingEmptySubsequences: true)
            guard parts.count == 3, let status = Result.Status(rawValue: String(parts[1])) else { return nil }
            return Result(key: String(parts[0]), status: status, detail: String(parts[2]))
        }
    }

    /// Whether the results show the machine isolated: no probe failed.
    static func passed(_ results: [Result]) -> Bool {
        !results.isEmpty && !results.contains { $0.status == .fail }
    }
}

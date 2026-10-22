/// Navigation item in sidebar
enum NavItem: String, CaseIterable, Identifiable {
    case activity
    case containers
    case volumes
    case images
    case networks
    case pods
    case services
    case machines
    case sandboxes
    case proxies

    var id: String { rawValue }

    var label: String {
        switch self {
        case .activity: "Activity"
        case .containers: "Containers"
        case .volumes: "Volumes"
        case .images: "Images"
        case .networks: "Networks"
        case .pods: "Pods"
        case .services: "Services"
        case .machines: "Machines"
        case .sandboxes: "Sandboxes"
        case .proxies: "Proxies"
        }
    }

    var sfSymbol: String {
        switch self {
        case .activity: "waveform.path.ecg"
        case .containers: "cube"
        case .volumes: "internaldrive"
        case .images: "circle.circle"
        case .networks: "point.3.filled.connected.trianglepath.dotted"
        case .pods: "helm"
        case .services: "gearshape.2"
        case .machines: "desktopcomputer"
        case .sandboxes: "server.rack"
        case .proxies: "network.badge.shield.half.filled"
        }
    }

    /// A page that fills the window beside the sidebar instead of a list and
    /// a detail column.
    var isFullWidth: Bool { self == .activity || self == .proxies }

    /// Deep links may name a resource in this section to select.
    var acceptsResourceID: Bool { !isFullWidth }

    /// Sidebar sections
    enum Section: String, CaseIterable, Identifiable {
        case system = "SYSTEM"
        case docker = "DOCKER"
        case kubernetes = "KUBERNETES"
        case linux = "LINUX"
        case sandbox = "SANDBOX"

        var id: String { rawValue }

        var items: [NavItem] {
            switch self {
            case .system: [.activity, .proxies]
            case .docker: [.containers, .volumes, .images, .networks]
            case .kubernetes: [.pods, .services]
            case .linux: [.machines]
            case .sandbox: [.sandboxes]
            }
        }
    }
}

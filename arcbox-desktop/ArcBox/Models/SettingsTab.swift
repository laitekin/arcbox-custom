enum SettingsTab: String, CaseIterable, Identifiable {
    case general = "General"
    case system = "System"
    case network = "Network"
    case storage = "Storage"

    var id: String { rawValue }

    var sfSymbol: String {
        switch self {
        case .general: "gearshape"
        case .system: "square.grid.2x2"
        case .network: "network"
        case .storage: "externaldrive"
        }
    }
}

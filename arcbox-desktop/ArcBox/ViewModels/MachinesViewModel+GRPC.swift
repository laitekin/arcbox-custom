import ArcBoxClient
import Foundation
import OSLog

/// Parameters for creating a machine.
struct MachineCreateSpec {
    var name = ""
    var distro = ""
    var version = ""
    var cpus: UInt32 = 0
    var memoryGiB: UInt64 = 4
    var diskGiB: UInt64 = 50
    /// A local image (`name@version`) to boot instead of a distro.
    var image = ""
    /// Empty inherits the default; `direct`, or a saved proxy ID.
    var proxyChoice = ""
    /// Isolate the machine from the Mac's files.
    var isolated = false
}

extension MachinesViewModel {
    // MARK: - gRPC Lifecycle Operations

    /// Load machines from the daemon via gRPC List.
    ///
    /// The `default` machine is the System VM hosting Docker/Kubernetes; its
    /// lifecycle belongs to the app, not this tab, so it is hidden here.
    func loadMachines(client: ArcBoxClient?) async {
        guard let client else {
            if loadState != .loaded {
                loadState = .waiting
            }
            return
        }

        await listLoadGate.run {
            await self.performLoadMachines(client: client)
        }
    }

    private func performLoadMachines(client: ArcBoxClient) async {
        let isRefresh = loadState.beginLoading()
        let transitioning = transitioningIDs
        let existingByID = Dictionary(uniqueKeysWithValues: machines.map { ($0.id, $0) })
        var request = Arcbox_V1_ListMachinesRequest()
        request.all = true
        do {
            let response = try await client.machines.list(
                request,
                options: ArcBoxClient.defaultCallOptions
            )
            var viewModels = response.machines
                .filter { $0.id != "default" }
                .map { summary -> MachineViewModel in
                    var vm = MachineViewModel(from: summary)
                    // Preserve Inspect-only fields so a list refresh does not
                    // wipe data the summary endpoint doesn't return.
                    if let existing = existingByID[vm.id] {
                        vm.preserveDetailFrom(existing)
                    }
                    return vm
                }
            for i in viewModels.indices where transitioning.contains(viewModels[i].id) {
                viewModels[i].isTransitioning = true
            }
            machines = viewModels
            loadState = .loaded
            refreshError = nil
            lastSuccessfulListLoad = ContinuousClock().now
        } catch {
            if loadState.cancelLoading(for: error, retainingLoadedContent: isRefresh) {
                return
            }
            let message = reportError(error, operation: "list", surface: false)
            refreshError = loadState.fail(message, retainingLoadedContent: isRefresh)
        }
    }

    /// Load Inspect-only details of one machine (arch, network, mounts).
    func loadMachineDetails(_ id: String, client: ArcBoxClient?) async {
        guard let client else { return }
        var request = Arcbox_V1_InspectMachineRequest()
        request.id = id
        do {
            let info = try await client.machines.inspect(
                request,
                options: ArcBoxClient.defaultCallOptions
            )
            updateMachine(id) { machine in
                machine.applyDetails(from: info)
            }
        } catch {
            reportError(error, operation: "inspect", surface: false)
        }
    }

    /// Create a machine from a published distro image and start it.
    /// Returns the new machine ID on success.
    @discardableResult
    func createMachine(_ spec: MachineCreateSpec, client: ArcBoxClient?) async -> String? {
        guard let client else {
            lastError = "ArcBox daemon is unavailable."
            return nil
        }
        var request = Arcbox_V1_CreateMachineRequest()
        request.name = spec.name
        request.distro = spec.distro
        request.version = spec.version
        request.cpus = spec.cpus
        request.memory = spec.memoryGiB << 30
        request.diskSize = spec.diskGiB << 30
        request.image = spec.image
        request.isolated = spec.isolated
        do {
            // Create pulls the distro image from the CDN on first use.
            let response = try await client.machines.create(
                request,
                options: ArcBoxClient.machineCreateCallOptions
            )
            Log.machine.info("Created machine \(response.id, privacy: .private(mask: .hash))")
            if !spec.proxyChoice.isEmpty {
                var assignment = Arcbox_V1_SetMachineProxyRequest()
                assignment.machine = response.id
                assignment.choice = spec.proxyChoice
                do {
                    _ = try await client.proxyLibrary.setMachineProxy(
                        assignment, options: ArcBoxClient.defaultCallOptions)
                } catch {
                    await loadMachines(client: client)
                    lastError =
                        "The machine was created and left stopped because its proxy could not be assigned. Choose a proxy in Info before starting it: "
                        + ArcBoxClient.userMessage(for: error)
                    return response.id
                }
            }
            await loadMachines(client: client)
            await startMachine(response.id, client: client)
            return response.id
        } catch {
            reportError(error, operation: "create")
            return nil
        }
    }

    /// Saves a machine's CPU and memory limits. Returns the daemon's answer,
    /// or `nil` after surfacing the error in `lastError`.
    func setMachineResources(
        _ id: String,
        draft: MachineResourceDraft,
        client: ArcBoxClient?
    ) async -> Arcbox_V1_SetMachineResourcesResponse? {
        guard let client else {
            lastError = "ArcBox daemon is unavailable."
            return nil
        }
        do {
            let response = try await client.machines.setResources(
                draft.request(machine: id),
                options: ArcBoxClient.defaultCallOptions
            )
            if response.restartRequired {
                resourceRestartPending.insert(id)
            }
            await loadMachines(client: client)
            return response
        } catch {
            reportError(error, operation: "set resources")
            return nil
        }
    }

    /// Isolates a stopped machine from the Mac's files, or lifts it; the
    /// change applies at its next start. Returns whether it was saved.
    @discardableResult
    func setMachineIsolation(_ id: String, isolated: Bool, client: ArcBoxClient?) async -> Bool {
        guard let client else {
            lastError = "ArcBox daemon is unavailable."
            return false
        }
        var request = Arcbox_V1_SetMachineIsolationRequest()
        request.id = id
        request.isolated = isolated
        do {
            _ = try await client.machines.setIsolation(request, options: ArcBoxClient.defaultCallOptions)
            updateMachine(id) { $0.isolated = isolated }
            await loadMachines(client: client)
            return true
        } catch {
            reportError(error, operation: "set isolation")
            return false
        }
    }

    /// Stops and starts a machine so it boots with its saved limits.
    func restartMachine(_ id: String, client: ArcBoxClient?) async {
        await stopMachine(id, client: client)
        guard lastError == nil else { return }
        await startMachine(id, client: client)
    }

    func startMachine(_ id: String, client: ArcBoxClient?) async {
        lastError = nil
        guard let client else { return }
        setTransitioning(id, true)
        var request = Arcbox_V1_StartMachineRequest()
        request.id = id
        do {
            _ = try await client.machines.start(
                request,
                options: ArcBoxClient.systemVmRestartCallOptions
            )
        } catch {
            // Starting an already-running machine is a no-op, not a failure.
            if !ArcBoxClient.rpcMessage(error, contains: "already running") {
                reportError(error, operation: "start")
            }
        }
        if lastError == nil {
            resourceRestartPending.remove(id)
        }
        setTransitioning(id, false)
        await loadMachines(client: client)
    }

    func stopMachine(_ id: String, client: ArcBoxClient?) async {
        lastError = nil
        guard let client else { return }
        setTransitioning(id, true)
        var request = Arcbox_V1_StopMachineRequest()
        request.id = id
        do {
            _ = try await client.machines.stop(
                request,
                options: ArcBoxClient.systemVmRestartCallOptions
            )
        } catch {
            // Stopping a machine that is not running is a no-op, not a failure.
            if !ArcBoxClient.rpcMessage(error, contains: "not running") {
                reportError(error, operation: "stop")
            }
        }
        if lastError == nil {
            resourceRestartPending.remove(id)
        }
        setTransitioning(id, false)
        await loadMachines(client: client)
    }

    func deleteMachine(_ id: String, client: ArcBoxClient?) async {
        lastError = nil
        guard let client else { return }
        setTransitioning(id, true)
        var request = Arcbox_V1_RemoveMachineRequest()
        request.id = id
        request.force = true
        do {
            _ = try await client.machines.remove(
                request,
                options: ArcBoxClient.systemVmRestartCallOptions
            )
            if selectedID == id {
                selectedID = nil
            }
        } catch {
            reportError(error, operation: "remove")
        }
        setTransitioning(id, false)
        await loadMachines(client: client)
    }
}

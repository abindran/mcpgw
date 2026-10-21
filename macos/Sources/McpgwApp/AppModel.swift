import AppKit
import Foundation
import McpgwKit
import Observation
import UserNotifications

/// Keys of the app's settings in `UserDefaults`.
enum Pref {
    /// `--config` path; empty uses mcpgw's default.
    static let configPath = "configPath"
    /// Start the daemon when the app opens.
    static let autoStart = "autoStart"
    /// Notify when a call waits for approval.
    static let notifyApprovals = "notifyApprovals"
    /// Stop the daemon when the app quits.
    static let stopOnQuit = "stopOnQuit"

    static func register() {
        UserDefaults.standard.register(defaults: [
            configPath: "", autoStart: true, notifyApprovals: true, stopOnQuit: false,
        ])
    }
}

/// The daemon as the app sees it.
enum Phase: Equatable {
    /// Looking for the binary and the daemon.
    case starting(String)
    case connected
    case stopped
    /// No `mcpgw` binary, or it can't be run.
    case broken(String)

    var label: String {
        switch self {
        case .starting(let s): s
        case .connected: "Running"
        case .stopped: "Stopped"
        case .broken: "Unavailable"
        }
    }
}

/// Everything the windows and the menu bar show, refreshed from the daemon
/// every two seconds while it's connected.
@MainActor @Observable
final class AppModel {
    private(set) var phase: Phase = .starting("Starting…")
    private(set) var mcpgw: MCPGW?
    private(set) var api: APIClient?

    private(set) var runtime: Runtime?
    private(set) var servers: [Server] = []
    private(set) var tools: [Tool] = []
    private(set) var calls: [Call] = []
    private(set) var approvals: [Approval] = []
    private(set) var profiles: [Profile] = []
    private(set) var secrets: [SecretMeta] = []
    private(set) var version: String?

    /// The last failed action, shown as a banner until dismissed.
    var notice: String?
    /// Set while a lifecycle action (start, stop) runs.
    private(set) var busy = false

    private var poller: Task<Void, Never>?
    private var notified = Set<String>()
    private var slowTick = 0
    private var shellPATH: String?

    var pendingApprovals: [Approval] { approvals.filter(\.pending) }
    var connected: Bool { phase == .connected }

    init() {
        Pref.register()
        Task { await boot() }
    }

    // MARK: lifecycle

    private func makeCLI() -> MCPGW? {
        var env = ProcessInfo.processInfo.environment
        guard let bin = MCPGW.locate(environment: env) else { return nil }
        let extra = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        env["PATH"] = MCPGW.mergePATH(shellPATH ?? env["PATH"], extra)
        let cfg = UserDefaults.standard.string(forKey: Pref.configPath) ?? ""
        return MCPGW(binary: bin, config: cfg.isEmpty ? nil : (cfg as NSString).expandingTildeInPath, environment: env)
    }

    private func boot() async {
        phase = .starting("Reading your shell's PATH…")
        shellPATH = await MCPGW.loginShellPATH()
        await reconnect(start: UserDefaults.standard.bool(forKey: Pref.autoStart))
        if UserDefaults.standard.bool(forKey: Pref.notifyApprovals), Bundle.main.bundleIdentifier != nil {
            _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
        }
    }

    /// Find the daemon again, starting it when `start` is set. Called on
    /// launch, when the config setting changes, and after a lost connection.
    func reconnect(start: Bool = false) async {
        guard let cli = makeCLI() else {
            mcpgw = nil
            phase = .broken("The mcpgw binary isn't in the app bundle or on PATH. Build the app with macos/build.sh, or set MCPGW_BIN.")
            return
        }
        mcpgw = cli
        if version == nil {
            version = try? await cli.check(["--version"], timeout: 10).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        phase = .starting("Looking for the daemon…")
        do {
            var client = try await cli.connect()
            if client == nil && start {
                phase = .starting("Starting the daemon…")
                try await cli.up()
                client = try await cli.connect()
            }
            if let client {
                attach(client)
            } else {
                detach(.stopped)
            }
        } catch {
            detach(.broken(error.localizedDescription))
        }
    }

    func startDaemon() async {
        busy = true
        defer { busy = false }
        await reconnect(start: true)
    }

    func stopDaemon() async {
        guard let cli = mcpgw else { return }
        busy = true
        defer { busy = false }
        do {
            try await cli.down()
            detach(.stopped)
        } catch {
            notice = error.localizedDescription
        }
    }

    func restartDaemon() async {
        await stopDaemon()
        await startDaemon()
    }

    private func attach(_ client: APIClient) {
        api = client
        phase = .connected
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func detach(_ next: Phase) {
        poller?.cancel()
        poller = nil
        if next == .stopped { watchForDaemon() }
        api = nil
        runtime = nil
        servers = []
        tools = []
        calls = []
        approvals = []
        profiles = []
        secrets = []
        phase = next
    }

    /// While stopped, notice a daemon started elsewhere (`mcpgw up` in a
    /// terminal, a client's `mcpgw connect`).
    private func watchForDaemon() {
        poller = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                guard let self else { return }
                guard !Task.isCancelled, phase == .stopped, !busy, let client = try? await mcpgw?.connect()
                else { continue }
                if phase == .stopped && !busy { attach(client) }
                return
            }
        }
    }

    /// Stop the daemon if the settings say so (called on quit).
    func quitting() async {
        if UserDefaults.standard.bool(forKey: Pref.stopOnQuit), connected { try? await mcpgw?.down() }
    }

    // MARK: refresh

    func refresh() async {
        guard let api else { return }
        do {
            async let rt = api.runtime()
            async let s = api.servers()
            async let a = api.approvals()
            async let c = api.calls(limit: 100)
            async let t = api.tools()
            let (r, srv, ap, cl, tl) = try await (rt, s, a, c, t)
            runtime = r
            if servers != srv { servers = srv }
            if approvals != ap { approvals = ap; notify(ap) }
            if calls != cl { calls = cl }
            if tools != tl { tools = tl }
            if slowTick % 5 == 0 || profiles.isEmpty { await refreshSlow() }
            slowTick += 1
            NSApp?.dockTile.badgeLabel = pendingApprovals.isEmpty ? nil : "\(pendingApprovals.count)"
        } catch APIError.unreachable {
            // the daemon went away (stopped from a terminal, crashed) or
            // came back on another port: ask mcpgw again
            detach(.starting("Reconnecting…"))
            NSApp?.dockTile.badgeLabel = nil
            await reconnect()
        } catch {
            notice = error.localizedDescription
        }
    }

    /// Profiles and secrets change rarely: read them every tenth second.
    func refreshSlow() async {
        guard let api else { return }
        if let p = try? await api.profiles(), p != profiles { profiles = p }
        if let s = try? await api.secrets(), s != secrets { secrets = s }
    }

    private func notify(_ list: [Approval]) {
        guard UserDefaults.standard.bool(forKey: Pref.notifyApprovals), Bundle.main.bundleIdentifier != nil else { return }
        for a in list where a.pending && !notified.contains(a.id) {
            notified.insert(a.id)
            let c = UNMutableNotificationContent()
            c.title = "Approve \(a.tool)?"
            c.body = "\(a.endpoint)\(a.client.map { " · \($0)" } ?? ""): \(a.arguments)"
            c.sound = .default
            UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: a.id, content: c, trigger: nil))
        }
    }

    // MARK: actions

    /// Run an API action, report its failure, refresh either way.
    @discardableResult
    func perform(_ what: @escaping (APIClient) async throws -> Void) async -> Bool {
        guard let api else { return false }
        do {
            try await what(api)
            notice = nil
            await refresh()
            await refreshSlow()
            return true
        } catch {
            notice = error.localizedDescription
            await refresh()
            return false
        }
    }

    func decide(_ a: Approval, approve: Bool) async {
        await perform { try await $0.decide(a.id, approve: approve) }
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [a.id])
    }

    /// `mcpgw add …`; returns what it printed.
    func addServer(_ form: AddServer) async throws -> String {
        guard let cli = mcpgw else { throw AddServerError(message: "mcpgw isn't available.") }
        let args = try form.cliArguments().get()
        let out = try await cli.check(args, timeout: 900)
        await refresh()
        return out
    }

    /// `mcpgw client install <client>`; returns what it printed.
    func installClient(_ client: String, profile: String?) async throws -> String {
        guard let cli = mcpgw else { throw AddServerError(message: "mcpgw isn't available.") }
        var args = ["client", "install", client]
        if let profile, !profile.isEmpty { args += ["--profile", profile] }
        return try await cli.check(args, timeout: 60)
    }

    func doctor() async throws -> [DoctorCheck] {
        guard let cli = mcpgw else { throw AddServerError(message: "mcpgw isn't available.") }
        return try await cli.doctor()
    }

    func openWebUI() {
        guard let api else { return }
        NSWorkspace.shared.open(api.webUIURL)
    }

    /// Link `~/.local/bin/mcpgw` to the bundled binary, so terminals use the
    /// same version as the app.
    func installCommandLineTool() -> String {
        guard let bin = mcpgw?.binary else { return "mcpgw isn't available." }
        let dir = URL(fileURLWithPath: NSHomeDirectory()).appending(path: ".local/bin")
        let link = dir.appending(path: "mcpgw")
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
            if let dest = try? fm.destinationOfSymbolicLink(atPath: link.path) {
                if dest == bin.path { return "\(link.path) already points at this app's mcpgw." }
                try fm.removeItem(at: link)
            } else if fm.fileExists(atPath: link.path) {
                return "\(link.path) exists and isn't a link; remove it first."
            }
            try fm.createSymbolicLink(at: link, withDestinationURL: bin)
            let onPath = (shellPATH ?? "").split(separator: ":").contains { $0 == dir.path }
            return "Linked \(link.path) → \(bin.path)." + (onPath ? "" : " Add \(dir.path) to your PATH to use it.")
        } catch {
            return error.localizedDescription
        }
    }
}

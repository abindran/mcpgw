import Foundation

// What `/api/v1` returns (see docs/reference/api.md). Keys are snake_case
// on the wire. Each type maps them in `CodingKeys` rather than through
// `.convertFromSnakeCase`, which would also rewrite the keys inside
// free-form JSON such as tool schemas.

/// `GET /runtime`.
public struct Runtime: Codable, Hashable, Sendable {
    public struct Counts: Codable, Hashable, Sendable {
        public var total: Int
        public var running: Int
    }

    public var version: String
    public var pid: Int
    public var startedAt: Int
    public var uptimeSecs: Int
    public var approvalsPending: Int
    public var servers: Counts
    public var endpoints: [String]

    enum CodingKeys: String, CodingKey {
        case version
        case pid
        case startedAt = "started_at"
        case uptimeSecs = "uptime_secs"
        case approvalsPending = "approvals_pending"
        case servers
        case endpoints
    }
}

/// One entry of `GET /servers`.
public struct Server: Codable, Hashable, Sendable, Identifiable {
    public struct Container: Codable, Hashable, Sendable {
        public var runtime: String
        public var name: String
        public var image: String
        public var rootless: Bool?
    }

    public var name: String
    public var transport: String
    public var target: String
    public var alias: String?
    public var enabled: Bool
    public var stateful: Bool
    public var source: String
    public var state: String?
    public var pid: Int?
    public var restarts: Int?
    public var warm: Bool
    public var coldStarts: Int?
    public var lastStartMs: Int?
    public var prewarms: Int?
    public var container: Container?
    public var protocolVersion: String?
    public var lastError: String?
    public var tools: Int
    public var pin: String?

    public var id: String { name }

    /// The state the web UI shows: `cold` until the server first starts.
    public var displayState: String { state ?? (enabled ? "cold" : "disabled") }

    /// Servers from the config file can't be changed through the API.
    public var fromConfig: Bool { source == "config" }

    /// The tool-name prefix: the alias, else the name.
    public var prefix: String { alias ?? name }

    enum CodingKeys: String, CodingKey {
        case name
        case transport
        case target
        case alias
        case enabled
        case stateful
        case source
        case state
        case pid
        case restarts
        case warm
        case coldStarts = "cold_starts"
        case lastStartMs = "last_start_ms"
        case prewarms
        case container
        case protocolVersion = "protocol_version"
        case lastError = "last_error"
        case tools
        case pin
    }
}

/// A public tool (`GET /tools`), named `<server>__<tool>`.
public struct Tool: Codable, Hashable, Sendable, Identifiable {
    public var name: String
    public var description: String?
    public var inputSchema: JSONValue?
    public var server: String?

    public var id: String { name }
}

/// One entry of `GET /calls`.
public struct Call: Codable, Hashable, Sendable, Identifiable {
    public var atMs: Int
    public var endpoint: String
    public var tool: String
    public var outcome: String
    public var durationMs: Int
    public var client: String?

    public var id: String { "\(atMs)-\(tool)-\(endpoint)-\(durationMs)" }
    public var date: Date { Date(timeIntervalSince1970: Double(atMs) / 1000) }

    enum CodingKeys: String, CodingKey {
        case atMs = "at_ms"
        case endpoint
        case tool
        case outcome
        case durationMs = "duration_ms"
        case client
    }
}

/// One entry of `GET /approvals`.
public struct Approval: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var tool: String
    public var arguments: String
    public var endpoint: String
    public var client: String?
    public var createdAt: Int
    public var status: String

    public var pending: Bool { status == "pending" }

    enum CodingKeys: String, CodingKey {
        case id
        case tool
        case arguments
        case endpoint
        case client
        case createdAt = "created_at"
        case status
    }
}

/// One entry of `GET /profiles`.
public struct Profile: Codable, Hashable, Sendable, Identifiable {
    public var name: String
    public var servers: [String]?
    public var mode: String?
    public var pinnedTools: [String]?
    public var accounts: [String: String]?
    public var policy: JSONValue?
    public var source: String
    public var url: String

    public var id: String { name }
    public var fromConfig: Bool { source == "config" }

    enum CodingKeys: String, CodingKey {
        case name
        case servers
        case mode
        case pinnedTools = "pinned_tools"
        case accounts
        case policy
        case source
        case url
    }
}

/// One entry of `GET /secrets` (metadata only; values never leave the vault).
public struct SecretMeta: Codable, Hashable, Sendable, Identifiable {
    public var name: String
    public var description: String?
    public var createdAt: Int
    public var updatedAt: Int
    public var lastUsedAt: Int?

    public var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name
        case description
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case lastUsedAt = "last_used_at"
    }
}

/// One entry of `GET /audit`.
public struct AuditEntry: Codable, Hashable, Sendable, Identifiable {
    public var seq: Int
    public var atMs: Int
    public var kind: String
    public var actor: String
    public var subject: String
    public var argsDigest: String?
    public var detail: JSONValue?
    public var prevHash: String
    public var hash: String

    public var id: Int { seq }
    public var date: Date { Date(timeIntervalSince1970: Double(atMs) / 1000) }

    enum CodingKeys: String, CodingKey {
        case seq
        case atMs = "at_ms"
        case kind
        case actor
        case subject
        case argsDigest = "args_digest"
        case detail
        case prevHash = "prev_hash"
        case hash
    }
}

/// `GET /audit/verify`.
public struct AuditVerify: Codable, Hashable, Sendable {
    public var ok: Bool
    public var entries: Int?
    public var brokenAt: Int?
    public var reason: String?

    enum CodingKeys: String, CodingKey {
        case ok
        case entries
        case brokenAt = "broken_at"
        case reason
    }
}

/// `POST /servers/{name}/pin`.
public struct Pin: Codable, Hashable, Sendable {
    public var sha256: String
    public var tools: Int
}

/// A `CallToolResult`, reduced to what the inspector shows.
public struct CallResult: Sendable, Hashable {
    public var raw: JSONValue
    public var isError: Bool { raw["isError"]?.bool ?? false }

    /// The text parts joined, or the whole result when there are none.
    public var text: String {
        let parts = (raw["content"]?.array ?? []).map { c in c["text"]?.string ?? c.compact() }
        if parts.isEmpty { return raw.pretty() }
        return parts.joined(separator: "\n")
    }

    public init(raw: JSONValue) { self.raw = raw }
}

/// One finding of `mcpgw doctor --json`.
public struct DoctorCheck: Codable, Hashable, Sendable {
    // `id` names the kind of check and repeats across subjects, so this
    // isn't `Identifiable`.
    public var id: String
    public var subject: String
    /// `ok`, `info`, `warn` or `fail`.
    public var level: String
    public var message: String
    public var fix: String?
}

/// Whether an MCP client goes through mcpgw, as `mcpgw doctor` sees its
/// user config (the `client <name>` checks).
public enum ClientStatus: Equatable, Sendable {
    /// Its config has an mcpgw entry that runs and uses this config.
    case connected
    /// It has servers of its own, none through mcpgw.
    case notConnected
    /// Its mcpgw entry runs a binary that's gone (e.g. the app moved).
    case broken(String)
    /// Its mcpgw entry uses another `--config`.
    case otherConfig(String)
    /// Its config can't be read.
    case unreadable(String)
    /// No config file (not installed, or never set up).
    case noConfig

    /// The status of `client` (`claude-desktop`, `cursor`, …) from a
    /// doctor report. With several mcpgw entries, the worst one wins.
    public static func from(_ checks: [DoctorCheck], client: String) -> ClientStatus {
        let mine = checks.filter { $0.subject == "client \(client)" }
        if let c = mine.first(where: { $0.id == "client_config_unreadable" }) { return .unreadable(c.message) }
        if let c = mine.first(where: { $0.id == "client_entry_broken" }) { return .broken(c.message) }
        if let c = mine.first(where: { $0.id == "client_other_config" }) { return .otherConfig(c.message) }
        if mine.contains(where: { $0.id == "client" }) { return .connected }
        if mine.contains(where: { $0.id == "client_not_using_mcpgw" }) { return .notConnected }
        return .noConfig
    }

    public var connected: Bool { self == .connected }
}

/// One entry of `GET /projects`: a `.mcpgw.toml` and what it runs.
public struct Project: Codable, Hashable, Sendable, Identifiable {
    public struct ServerInfo: Codable, Hashable, Sendable, Identifiable {
        /// The name in the file (its tools' prefix).
        public var name: String
        /// The internal server name, `<name>-p<id>`.
        public var server: String
        /// What it runs, and the secrets it gets.
        public var runs: String
        public var state: String?
        public var tools: Int

        public var id: String { server }

        /// The transport, from what it runs.
        public var kind: String {
            if runs.hasPrefix("runs container") { return "container" }
            if runs.hasPrefix("runs Wasm") { return "wasm" }
            if runs.hasPrefix("runs ") { return "stdio" }
            if runs.hasPrefix("connects to") { return "http" }
            if runs.hasPrefix("calls the API") { return "openapi" }
            return "?"
        }
    }

    public var id: String
    public var name: String
    public var root: String
    /// `trusted`, `untrusted`, `changed`, `invalid` or `missing`.
    public var state: String
    public var error: String?
    public var sha256: String?
    public var trustedSha256: String?
    public var url: String
    public var servers: [ServerInfo]

    enum CodingKeys: String, CodingKey {
        case id, name, root, state, error, sha256, url, servers
        case trustedSha256 = "trusted_sha256"
    }

    /// Its servers are off until the user trusts the file.
    public var needsTrust: Bool { (state == "untrusted" || state == "changed") && !servers.isEmpty }
}

/// A project's use of a server it declares.
public struct ProjectUse: Hashable, Sendable, Identifiable {
    public var project: Project
    public var server: Project.ServerInfo
    public var id: String { server.server }

    /// Its state: off while the project isn't trusted.
    public var state: String { server.state ?? (project.state == "trusted" ? "cold" : "off") }

    public init(project: Project, server: Project.ServerInfo) {
        self.project = project
        self.server = server
    }
}

/// A row of the Servers screen: a global server, which every endpoint
/// serves, or a server that projects declare in their `.mcpgw.toml`, once
/// per name however many projects use it.
public enum ServerRow: Hashable, Sendable, Identifiable {
    case global(Server)
    case project(name: String, uses: [ProjectUse])

    public var id: String {
        switch self {
        case .global(let s): "global:\(s.name)"
        case .project(let n, _): "project:\(n)"
        }
    }

    public var name: String {
        switch self {
        case .global(let s): s.name
        case .project(let n, _): n
        }
    }

    /// The most telling state among a project server's instances.
    public var state: String {
        switch self {
        case .global(let s): return s.displayState
        case .project(_, let uses):
            let order = ["quarantined", "failed", "backoff", "busy", "starting", "draining", "ready", "idle", "cold", "disabled", "off"]
            return uses.map(\.state).min { (order.firstIndex(of: $0) ?? 99) < (order.firstIndex(of: $1) ?? 99) } ?? "off"
        }
    }

    /// Global servers, then project servers grouped by name.
    public static func rows(servers: [Server], projects: [Project]) -> [ServerRow] {
        var byName: [String: [ProjectUse]] = [:]
        for p in projects {
            for s in p.servers { byName[s.name, default: []].append(ProjectUse(project: p, server: s)) }
        }
        let grouped = byName.keys.sorted().map { n in
            ServerRow.project(name: n, uses: byName[n]!.sorted { $0.project.name < $1.project.name })
        }
        return servers.map(ServerRow.global) + grouped
    }
}

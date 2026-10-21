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

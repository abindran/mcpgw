import Foundation

/// An error from the daemon or from reaching it.
public enum APIError: Error, LocalizedError, Sendable, Equatable {
    /// The daemon answered `{"error": …}` with this status.
    case server(status: Int, message: String)
    /// The token was rejected (401).
    case unauthorized
    /// No answer: the daemon isn't running or the connection failed.
    case unreachable(String)
    /// The answer wasn't what this app expects.
    case decoding(String)

    public var errorDescription: String? {
        switch self {
        case .server(_, let m): m
        case .unauthorized: "The daemon rejected the token. Reconnect to read it again."
        case .unreachable(let m): "Can't reach the daemon: \(m)"
        case .decoding(let m): "Unexpected answer from the daemon: \(m)"
        }
    }
}

/// The daemon's admin API (`/api/v1`), with its loopback bearer token.
public struct APIClient: Sendable {
    /// The daemon's base URL, e.g. `http://127.0.0.1:7777`.
    public let base: URL
    let token: String
    let session: URLSession

    public init(base: URL, token: String, timeout: TimeInterval = 30) {
        self.base = base
        self.token = token
        let cfg = URLSessionConfiguration.ephemeral
        cfg.timeoutIntervalForRequest = timeout
        cfg.timeoutIntervalForResource = 3600
        cfg.requestCachePolicy = .reloadIgnoringLocalCacheData
        session = URLSession(configuration: cfg)
    }

    /// The default MCP endpoint, `<base>/mcp`.
    public var mcpURL: URL { URL(string: base.absoluteString + "/mcp")! }

    /// The web UI, signed in through the URL fragment as `mcpgw ui` does.
    public var webUIURL: URL { URL(string: "\(base.absoluteString)/ui#token=\(token)")! }

    // MARK: requests

    func request(_ method: String, _ path: String, query: [URLQueryItem] = [], body: JSONValue? = nil,
                 timeout: TimeInterval? = nil) async throws -> Data? {
        // `path` is already percent-encoded (see `seg`), so it's joined as text
        var comps = URLComponents(string: base.absoluteString + "/api/v1" + path)!
        if !query.isEmpty { comps.queryItems = query }
        var req = URLRequest(url: comps.url!)
        req.httpMethod = method
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let timeout { req.timeoutInterval = timeout }
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONEncoder().encode(body)
        }
        let data: Data
        let resp: URLResponse
        do {
            (data, resp) = try await session.data(for: req)
        } catch {
            throw APIError.unreachable(error.localizedDescription)
        }
        let status = (resp as? HTTPURLResponse)?.statusCode ?? 0
        if status == 401 { throw APIError.unauthorized }
        guard (200..<300).contains(status) else {
            let msg = (try? JSONValue.parse(String(decoding: data, as: UTF8.self)))?["error"]?.string
            throw APIError.server(status: status, message: msg ?? HTTPURLResponse.localizedString(forStatusCode: status))
        }
        return status == 204 ? nil : data
    }

    func get<T: Decodable>(_ type: T.Type, _ path: String, query: [URLQueryItem] = []) async throws -> T {
        try decode(type, try await request("GET", path, query: query))
    }

    func decode<T: Decodable>(_ type: T.Type, _ data: Data?) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: data ?? Data("null".utf8))
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }

    static func seg(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_.~"))) ?? s
    }

    private struct Wrapped<T: Decodable>: Decodable {
        let value: T
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: AnyKey.self)
            guard let key = c.allKeys.first else {
                throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "empty object"))
            }
            value = try c.decode(T.self, forKey: key)
        }
    }

    private struct AnyKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    /// A `{"<key>": [...]}` list.
    func list<T: Decodable>(_ type: T.Type, _ path: String, query: [URLQueryItem] = []) async throws -> [T] {
        try await get(Wrapped<[T]>.self, path, query: query).value
    }

    // MARK: endpoints

    public func runtime() async throws -> Runtime { try await get(Runtime.self, "/runtime") }
    public func servers() async throws -> [Server] { try await list(Server.self, "/servers") }

    /// One server's status: a global server, or a project's (`<name>-p<id>`).
    public func serverStatus(_ name: String) async throws -> Server {
        try await get(Server.self, "/servers/\(Self.seg(name))")
    }

    public func server(_ name: String) async throws -> JSONValue {
        try await get(JSONValue.self, "/servers/\(Self.seg(name))")
    }

    public func setEnabled(_ name: String, _ on: Bool) async throws {
        _ = try await request("POST", "/servers/\(Self.seg(name))/\(on ? "enable" : "disable")")
    }

    public func restart(_ name: String) async throws {
        _ = try await request("POST", "/servers/\(Self.seg(name))/restart")
    }

    public func pin(_ name: String) async throws -> Pin {
        try decode(Pin.self, try await request("POST", "/servers/\(Self.seg(name))/pin"))
    }

    public func removeServer(_ name: String) async throws {
        _ = try await request("DELETE", "/servers/\(Self.seg(name))")
    }

    public func logs(_ name: String, lines: Int = 200) async throws -> [String] {
        try await list(String.self, "/servers/\(Self.seg(name))/logs", query: [.init(name: "lines", value: "\(lines)")])
    }

    /// A server's pinned tools (names as the server gives them, unprefixed).
    public func serverTools(_ name: String) async throws -> [Tool] {
        struct R: Decodable { let tools: [Tool] }
        return try await get(R.self, "/servers/\(Self.seg(name))/tools").tools
    }

    public func tools(profile: String? = nil) async throws -> [Tool] {
        try await list(Tool.self, "/tools", query: profile.map { [.init(name: "profile", value: $0)] } ?? [])
    }

    /// Call a tool as the default endpoint does: policy, approvals and audit apply.
    public func callTool(_ name: String, arguments: JSONValue) async throws -> CallResult {
        let data = try await request("POST", "/tools/\(Self.seg(name))/call", body: .object(["arguments": arguments]),
                                     timeout: 3600)
        return CallResult(raw: try decode(JSONValue.self, data))
    }

    public func calls(limit: Int = 100) async throws -> [Call] {
        try await list(Call.self, "/calls", query: [.init(name: "limit", value: "\(limit)")])
    }

    public func approvals() async throws -> [Approval] { try await list(Approval.self, "/approvals") }

    public func decide(_ id: String, approve: Bool) async throws {
        _ = try await request("POST", "/approvals/\(Self.seg(id))/\(approve ? "approve" : "deny")")
    }

    public func profiles() async throws -> [Profile] { try await list(Profile.self, "/profiles") }

    public func removeProfile(_ name: String) async throws {
        _ = try await request("DELETE", "/profiles/\(Self.seg(name))")
    }

    public func projects() async throws -> [Project] { try await list(Project.self, "/projects") }

    /// Register the project at `path` (absolute), or read it again.
    public func registerProject(path: String) async throws -> Project {
        try decode(Project.self, try await request("POST", "/projects", body: .object(["path": .string(path)])))
    }

    public func project(_ id: String) async throws -> Project {
        try await get(Project.self, "/projects/\(Self.seg(id))")
    }

    /// Trust the file's content: only if it's still `sha256` (what was shown).
    public func trustProject(_ id: String, sha256: String?) async throws -> Project {
        let body: JSONValue = .object(sha256.map { ["sha256": .string($0)] } ?? [:])
        return try decode(Project.self, try await request("POST", "/projects/\(Self.seg(id))/trust", body: body))
    }

    public func untrustProject(_ id: String) async throws -> Project {
        try decode(Project.self, try await request("POST", "/projects/\(Self.seg(id))/untrust"))
    }

    public func forgetProject(_ id: String) async throws {
        _ = try await request("DELETE", "/projects/\(Self.seg(id))")
    }

    public func secrets() async throws -> [SecretMeta] { try await list(SecretMeta.self, "/secrets") }

    public func setSecret(_ name: String, value: String, description: String?) async throws {
        var body: [String: JSONValue] = ["value": .string(value)]
        if let description, !description.isEmpty { body["description"] = .string(description) }
        _ = try await request("PUT", "/secrets/\(Self.seg(name))", body: .object(body))
    }

    public func removeSecret(_ name: String) async throws {
        _ = try await request("DELETE", "/secrets/\(Self.seg(name))")
    }

    public func verifyAudit() async throws -> AuditVerify { try await get(AuditVerify.self, "/audit/verify") }

    /// Up to `limit` entries after sequence number `after`, oldest first.
    public func audit(after: Int = 0, limit: Int = 200) async throws -> [AuditEntry] {
        try await list(AuditEntry.self, "/audit", query: [.init(name: "after", value: "\(after)"),
                                                           .init(name: "limit", value: "\(limit)")])
    }

    /// Whether `/healthz` answers.
    public func healthy() async -> Bool {
        var req = URLRequest(url: URL(string: base.absoluteString + "/healthz")!)
        req.timeoutInterval = 2
        guard let (data, resp) = try? await session.data(for: req) else { return false }
        return (resp as? HTTPURLResponse)?.statusCode == 200 && String(decoding: data, as: UTF8.self) == "ok"
    }
}

import Foundation
import Testing
@testable import McpgwKit

/// Drives a real daemon the way the app does: through the `mcpgw` binary
/// (status, token, up, add, down) and the admin API. Every endpoint the app
/// reads is decoded, so a change to the API's JSON fails here.
///
/// Needs the Rust binaries (`cargo build -p mcpgw -p mcpgw-testkit`):
/// `MCPGW_BIN` and `FAKE_MCP_BIN`, defaulting to `../target/debug/`. Skipped
/// without them unless `MCPGW_MAC_E2E=1`.
struct Binaries {
    let mcpgw: URL
    let fake: URL

    static func find() -> Binaries? {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appending(path: "../../../target/debug").standardized
        let mcpgw = env["MCPGW_BIN"].map { URL(fileURLWithPath: $0) } ?? root.appending(path: "mcpgw")
        let fake = env["FAKE_MCP_BIN"].map { URL(fileURLWithPath: $0) } ?? root.appending(path: "fake-mcp")
        let fm = FileManager.default
        guard fm.isExecutableFile(atPath: mcpgw.path), fm.isExecutableFile(atPath: fake.path) else {
            precondition(env["MCPGW_MAC_E2E"] != "1", "MCPGW_MAC_E2E=1 but \(mcpgw.path) or \(fake.path) is missing")
            return nil
        }
        return Binaries(mcpgw: mcpgw, fake: fake)
    }
}

@Test(.enabled(if: Binaries.find() != nil, "the mcpgw and fake-mcp binaries aren't built"), .timeLimit(.minutes(5)))
func drivesARealDaemon() async throws {
    let bins = Binaries.find()!
    let dir = FileManager.default.temporaryDirectory.appending(path: "mcpgw-mac-\(ProcessInfo.processInfo.processIdentifier)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: dir) }
    let cfg = dir.appending(path: "config.toml")
    try """
    [gateway]
    mode = "direct"
    store_path = "\(dir.appending(path: "db.sqlite").path)"
    token_path = "\(dir.appending(path: "token").path)"
    listen = "127.0.0.1:0"

    [[profile]]
    name = "narrow"
    servers = ["echo"]
    mode = "direct"
    policy = { deny = ["echo__crash"] }
    """.write(to: cfg, atomically: true, encoding: .utf8)

    var env = ProcessInfo.processInfo.environment
    env["HOME"] = dir.path
    env["MCPGW_VAULT_PASSPHRASE"] = "mac-test-passphrase"
    env.removeValue(forKey: "MCPGW_CONFIG")
    let cli = MCPGW(binary: bins.mcpgw, config: cfg.path, environment: env)

    #expect(try await cli.status() == .stopped)
    #expect(try await cli.connect() == nil)

    let added = try await cli.check(AddServer.with(name: "echo", command: bins.fake.path).cliArguments().get())
    #expect(added.contains("added server echo"))

    try await cli.up()
    defer { _ = try? Process.run(bins.mcpgw, ["--config", cfg.path, "down"], env) }
    let api = try #require(try await cli.connect())
    #expect(await api.healthy())

    let rt = try await api.runtime()
    #expect(rt.servers.total == 1)
    #expect(rt.endpoints.contains { $0.hasSuffix("/p/narrow") })

    let servers = try await api.servers()
    let echo = try #require(servers.first { $0.name == "echo" })
    #expect(echo.tools > 0 && echo.source == "store" && echo.transport == "stdio")

    let tools = try await api.tools()
    #expect(tools.contains { $0.name == "echo__echo" && $0.server == "echo" })
    #expect(try await api.tools(profile: "narrow").contains { $0.name == "echo__crash" } == false)
    #expect(try await api.serverTools("echo").contains { $0.name == "add" })

    let r = try await api.callTool("echo__add", arguments: .object(["a": .number(2), "b": .number(3)]))
    #expect(!r.isError && r.text == "5")
    let calls = try await api.calls()
    #expect(calls.first?.tool == "echo__add" && calls.first?.outcome == "ok")
    _ = try await api.logs("echo")
    let detail = try await api.server("echo")
    #expect(detail["spec"]?["name"]?.string == "echo")

    try await api.setEnabled("echo", false)
    #expect(try await api.servers().first?.displayState == "disabled")
    try await api.setEnabled("echo", true)
    try await api.restart("echo")

    try await api.setSecret("mac_test", value: "v-\(UUID().uuidString)", description: "from the mac tests")
    #expect(try await api.secrets().map(\.name) == ["mac_test"])
    try await api.removeSecret("mac_test")
    #expect(try await api.secrets().isEmpty)

    let profiles = try await api.profiles()
    #expect(profiles.map(\.name) == ["narrow"] && profiles[0].fromConfig)
    #expect(try await api.approvals().isEmpty)

    let v = try await api.verifyAudit()
    #expect(v.ok && (v.entries ?? 0) > 0)
    let audit = try await api.audit()
    #expect(audit.contains { $0.kind == "secret.set" })

    // a project: registered untrusted, trusted only as shown, then forgotten
    let proj = dir.appending(path: "proj")
    try FileManager.default.createDirectory(at: proj, withIntermediateDirectories: true)
    try """
    [project]
    name = "demo"

    [[server]]
    name = "local"
    transport = "stdio"
    command = "\(bins.fake.path)"
    """.write(to: proj.appending(path: ".mcpgw.toml"), atomically: true, encoding: .utf8)
    let root = proj.resolvingSymlinksInPath().path
    var p = try await api.registerProject(path: root)
    #expect(p.state == "untrusted" && p.needsTrust && p.servers.map(\.name) == ["local"])
    #expect(p.servers[0].runs.contains("runs \(bins.fake.path)"))
    #expect(try await api.projects().map(\.name) == ["demo"])
    await #expect(throws: APIError.self) { try await api.trustProject(p.id, sha256: String(repeating: "0", count: 64)) }
    p = try await api.trustProject(p.id, sha256: p.sha256)
    #expect(p.state == "trusted" && p.trustedSha256 == p.sha256 && !p.needsTrust)
    #expect(try await api.project(p.id).servers[0].tools > 0)
    // the Tools screen's project scope: listed and called as its endpoint
    let scoped = try await api.tools(project: p.id).map(\.name)
    #expect(scoped.contains("local__echo"))
    #expect(!(try await api.tools()).map(\.name).contains("local__echo"))
    let echoed = try await api.callTool("local__echo", arguments: .object(["text": .string("in demo")]), project: p.id)
    #expect(!echoed.isError && echoed.text.contains("in demo"))
    // its servers' state, as the Servers screen groups them
    let rows = ServerRow.rows(servers: try await api.servers(), projects: try await api.projects())
    #expect(rows.contains { $0.id == "project:local" })
    #expect(try await api.serverStatus(p.servers[0].server).tools > 0)
    #expect(try await api.untrustProject(p.id).state == "untrusted")
    try await api.forgetProject(p.id)
    #expect(try await api.projects().isEmpty)

    await #expect(throws: APIError.self) { try await api.removeServer("nope") }
    let checks = try await cli.doctor()
    #expect(!checks.isEmpty)

    try await cli.down()
    #expect(try await cli.status() == .stopped)
}

extension AddServer {
    static func with(name: String, command: String) -> AddServer {
        var f = AddServer()
        f.name = name
        f.target = command
        return f
    }
}

extension Process {
    static func run(_ exe: URL, _ args: [String], _ env: [String: String]) throws -> Process {
        let p = Process()
        p.executableURL = exe
        p.arguments = args
        p.environment = env
        try p.run()
        p.waitUntilExit()
        return p
    }
}

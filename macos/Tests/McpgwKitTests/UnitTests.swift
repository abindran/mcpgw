import Foundation
import Testing
@testable import McpgwKit

@Test func parsesRunningStatus() {
    let s = DaemonStatus.parse("daemon: running (pid 4242, http://127.0.0.1:7777/mcp, mcpgw 1.0.0)\necho\tready\t7 tools\tpid 9\n")
    #expect(s == .running(pid: 4242, mcpURL: URL(string: "http://127.0.0.1:7777/mcp")!, version: "1.0.0"))
}

@Test func parsesStoppedStatus() {
    #expect(DaemonStatus.parse("daemon: not running (start it with `mcpgw up`)\necho\tcold\n") == .stopped)
}

@Test func rejectsOtherStatusText() {
    #expect(DaemonStatus.parse("") == nil)
    #expect(DaemonStatus.parse("error: bad config") == nil)
    #expect(DaemonStatus.parse("daemon: running (pid x, y)") == nil)
}

@Test func splitsLikeAShell() throws {
    #expect(try ShellWords.split("-y @scope/pkg  ~/Documents").get() == ["-y", "@scope/pkg", "~/Documents"])
    #expect(try ShellWords.split(#"--name "two words" 'it''s' a\ b"#).get() == ["--name", "two words", "its", "a b"])
    #expect(try ShellWords.split(#""" ''"#).get() == ["", ""])
    #expect(try ShellWords.split("   ").get() == [])
    #expect(throws: AddServerError.self) { try ShellWords.split("'open").get() }
    #expect(throws: AddServerError.self) { try ShellWords.split("trailing\\").get() }
}

@Test func buildsAddArguments() throws {
    var f = AddServer()
    f.name = "fs"
    f.target = "npx"
    f.arguments = "-y @modelcontextprotocol/server-filesystem '/tmp/my dir'"
    f.pairs = "DEBUG=1\n\n LOG=info "
    f.secrets = "TOKEN=gh_token"
    f.alias = "files"
    f.warm = true
    #expect(try f.cliArguments().get() == [
        "add", "fs", "--command", "npx", "--env", "DEBUG=1", "--env", "LOG=info", "--secret", "TOKEN=gh_token",
        "--alias", "files", "--warm", "--", "-y", "@modelcontextprotocol/server-filesystem", "/tmp/my dir",
    ])

    var u = AddServer()
    u.kind = .url
    u.name = "gh"
    u.target = "https://example.test/mcp"
    u.pairs = "Authorization=Bearer ${secret:gh}"
    u.disabled = true
    #expect(try u.cliArguments().get() == ["add", "gh", "--url", "https://example.test/mcp",
                                            "--header", "Authorization=Bearer ${secret:gh}", "--disabled"])

    var r = AddServer()
    r.kind = .registry
    r.name = "io.github.github/github-mcp-server"
    #expect(try r.cliArguments().get() == ["add", "io.github.github/github-mcp-server", "--yes"])
}

@Test func refusesIncompleteForms() {
    var f = AddServer()
    #expect(throws: AddServerError.self) { try f.cliArguments().get() }
    f.name = "x"
    #expect(throws: AddServerError.self) { try f.cliArguments().get() }
    f.target = "cmd"
    f.pairs = "NOEQUALS"
    #expect(throws: AddServerError.self) { try f.cliArguments().get() }
    f.pairs = ""
    f.kind = .wasm
    f.arguments = "a b"
    #expect(throws: AddServerError.self) { try f.cliArguments().get() }
}

@Test func samplesArgumentsFromTheSchema() throws {
    let schema = try JSONValue.parse(#"""
    {"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"string"},"c":{"type":"boolean"},
     "d":{"type":["array","null"]},"e":{"type":"object"},"f":{}}}
    """#)
    #expect(sampleArguments(schema).compact() == #"{"a":0,"b":"","c":false,"d":[],"e":{},"f":""}"#)
    #expect(sampleArguments(nil) == .object([:]))
}

@Test func keepsJSONKeysAsTheyAre() throws {
    // tool schemas keep snake_case keys (no key conversion on decode)
    let t = try JSONDecoder().decode(Tool.self, from: Data(#"""
    {"name":"s__t","inputSchema":{"properties":{"max_items":{"type":"integer"}}},"server":"s"}
    """#.utf8))
    #expect(t.inputSchema?["properties"]?["max_items"] != nil)
    #expect(JSONValue.number(3).compact() == "3")
    #expect(JSONValue.number(2.5).compact() == "2.5")
}

@Test func readsCallResults() throws {
    let r = CallResult(raw: try JSONValue.parse(#"{"content":[{"type":"text","text":"a"},{"type":"image","data":"x"}],"isError":true}"#))
    #expect(r.isError)
    #expect(r.text == "a\n{\"data\":\"x\",\"type\":\"image\"}")
}

@Test func mergesPATH() {
    #expect(MCPGW.mergePATH("/a:/b", ["/b", "/c", ""]) == "/a:/b:/c")
    #expect(MCPGW.mergePATH(nil, ["/c"]) == "/c")
}

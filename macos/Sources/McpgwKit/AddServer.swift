import Foundation

/// What the "Add server" form collects, turned into `mcpgw add` arguments.
/// Adding goes through the CLI rather than `POST /servers`, so a new server
/// gets what `mcpgw add` does: its tools read and pinned, a poisoning scan,
/// npx/uvx packages cached, credentials moved to the vault.
public struct AddServer: Sendable, Equatable {
    public enum Kind: String, CaseIterable, Sendable, Identifiable {
        case command, url, image, wasm, openapi, registry

        public var id: String { rawValue }

        public var label: String {
            switch self {
            case .command: "Command (stdio)"
            case .url: "Remote URL"
            case .image: "Container image"
            case .wasm: "Wasm component"
            case .openapi: "OpenAPI spec"
            case .registry: "MCP Registry"
            }
        }
    }

    public var kind: Kind = .command
    /// Server name, or the registry entry for `.registry`.
    public var name = ""
    /// The command, URL, image, Wasm file or spec.
    public var target = ""
    /// Command or entrypoint arguments, split like a shell would.
    public var arguments = ""
    /// `KEY=VALUE` lines: env vars (command), headers (url).
    public var pairs = ""
    /// `VAR=SECRET` lines: vault secrets injected as env vars (command, image).
    public var secrets = ""
    public var alias = ""
    public var stateful = false
    public var warm = false
    public var disabled = false

    public init() {}

    /// The arguments after `mcpgw`, or why the form isn't complete.
    public func cliArguments() -> Result<[String], AddServerError> {
        let name = name.trimmingCharacters(in: .whitespaces)
        let target = target.trimmingCharacters(in: .whitespaces)
        if name.isEmpty { return .failure(.init(message: kind == .registry ? "Give the registry entry." : "Give the server a name.")) }
        if kind != .registry && target.isEmpty { return .failure(.init(message: "Fill in the \(kind.label.lowercased()).")) }
        var a = ["add", name]
        switch kind {
        case .command: a += ["--command", target]
        case .url: a += ["--url", target]
        case .image: a += ["--image", target]
        case .wasm: a += ["--wasm", target]
        case .openapi: a += ["--openapi", target]
        case .registry: a += ["--yes"]
        }
        let lines = { (s: String) in
            s.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        }
        for p in lines(pairs) {
            guard p.contains("=") else { return .failure(.init(message: "\"\(p)\" isn't KEY=VALUE.")) }
            switch kind {
            case .command: a += ["--env", p]
            case .url: a += ["--header", p]
            default: return .failure(.init(message: "Environment variables and headers apply to commands and URLs."))
            }
        }
        for s in lines(secrets) {
            guard s.contains("=") else { return .failure(.init(message: "\"\(s)\" isn't VAR=SECRET.")) }
            guard kind == .command || kind == .image else {
                return .failure(.init(message: "Secrets as env vars apply to commands and images."))
            }
            a += ["--secret", s]
        }
        let alias = alias.trimmingCharacters(in: .whitespaces)
        if !alias.isEmpty { a += ["--alias", alias] }
        if stateful { a.append("--stateful") }
        if warm { a.append("--warm") }
        if disabled { a.append("--disabled") }
        let args: [String]
        switch ShellWords.split(arguments) {
        case .success(let w): args = w
        case .failure(let e): return .failure(e)
        }
        if !args.isEmpty {
            guard kind == .command || kind == .image else {
                return .failure(.init(message: "Arguments apply to commands and images."))
            }
            a += ["--"] + args
        }
        return .success(a)
    }
}

public struct AddServerError: Error, Equatable, Sendable, LocalizedError {
    public var message: String
    public init(message: String) { self.message = message }
    public var errorDescription: String? { message }
}

/// Splitting a line into words the way a POSIX shell does for quotes and
/// backslashes (no expansion).
public enum ShellWords {
    public static func split(_ s: String) -> Result<[String], AddServerError> {
        var words: [String] = []
        var cur = ""
        var inWord = false
        var quote: Character?
        var escape = false
        for ch in s {
            if escape {
                cur.append(ch); escape = false; inWord = true
                continue
            }
            switch (quote, ch) {
            case ("'", "'"), ("\"", "\""): quote = nil
            case ("'", _): cur.append(ch)
            case ("\"", "\\"), (nil, "\\"): escape = true
            case ("\"", _): cur.append(ch)
            case (nil, "'"), (nil, "\""): quote = ch; inWord = true
            case (nil, _) where ch.isWhitespace:
                if inWord { words.append(cur); cur = ""; inWord = false }
            default: cur.append(ch); inWord = true
            }
        }
        if quote != nil { return .failure(.init(message: "Unclosed quote in the arguments.")) }
        if escape { return .failure(.init(message: "The arguments end with a backslash.")) }
        if inWord { words.append(cur) }
        return .success(words)
    }
}

/// Example arguments for a tool, from its input schema, as the web UI's
/// inspector builds them: each property gets an empty value of its type.
public func sampleArguments(_ schema: JSONValue?) -> JSONValue {
    var out: [String: JSONValue] = [:]
    for (k, v) in schema?["properties"]?.object ?? [:] {
        let type = v["type"]?.string ?? v["type"]?.array?.first?.string
        switch type {
        case "integer", "number": out[k] = .number(0)
        case "boolean": out[k] = .bool(false)
        case "object": out[k] = .object([:])
        case "array": out[k] = .array([])
        default: out[k] = .string("")
        }
    }
    return .object(out)
}

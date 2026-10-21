import Foundation

/// What a finished `mcpgw` command printed.
public struct CLIResult: Sendable, Equatable {
    public var status: Int32
    public var stdout: String
    public var stderr: String

    public var ok: Bool { status == 0 }

    /// stdout and stderr, trimmed, for showing to the user.
    public var output: String {
        [stdout, stderr].map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }.joined(separator: "\n")
    }
}

/// A failed `mcpgw` command.
public struct CLIError: Error, LocalizedError, Sendable {
    public var args: [String]
    public var result: CLIResult?
    public var message: String

    public var errorDescription: String? { message }
}

/// The daemon as `mcpgw status` reports it.
public enum DaemonStatus: Sendable, Equatable {
    case running(pid: Int, mcpURL: URL, version: String)
    case stopped

    /// Parse the first line of `mcpgw status`:
    /// `daemon: running (pid 42, http://127.0.0.1:7777/mcp, mcpgw 1.0.0)` or
    /// `daemon: not running (…)`.
    public static func parse(_ text: String) -> DaemonStatus? {
        guard let line = text.split(separator: "\n").first.map(String.init), line.hasPrefix("daemon: ") else {
            return nil
        }
        if line.hasPrefix("daemon: not running") { return .stopped }
        guard line.hasPrefix("daemon: running ("), line.hasSuffix(")") else { return nil }
        let inner = line.dropFirst("daemon: running (".count).dropLast()
        let parts = inner.components(separatedBy: ", ")
        guard parts.count == 3, parts[0].hasPrefix("pid "), let pid = Int(parts[0].dropFirst(4)),
              let url = URL(string: parts[1]), parts[2].hasPrefix("mcpgw ")
        else { return nil }
        return .running(pid: pid, mcpURL: url, version: String(parts[2].dropFirst(6)))
    }
}

/// Runs the `mcpgw` binary: the copy bundled in the app, or one found on disk.
public struct MCPGW: Sendable {
    /// The binary.
    public var binary: URL
    /// `--config` for every command; `nil` uses mcpgw's default
    /// (`$MCPGW_CONFIG` or `~/.config/mcpgw/config.toml`).
    public var config: String?
    /// The environment commands (and so the daemon they start) run with.
    public var environment: [String: String]

    public init(binary: URL, config: String? = nil, environment: [String: String] = ProcessInfo.processInfo.environment) {
        self.binary = binary
        self.config = config
        self.environment = environment
    }

    /// Where to find the binary: `$MCPGW_BIN`, next to the app's executable
    /// (`mcpgw.app/Contents/MacOS/mcpgw`), then `PATH` and the usual install
    /// locations.
    public static func locate(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL? {
        let fm = FileManager.default
        if let p = environment["MCPGW_BIN"], !p.isEmpty { return URL(fileURLWithPath: p) }
        var dirs: [String] = []
        if let exe = Bundle.main.executableURL { dirs.append(exe.deletingLastPathComponent().path) }
        dirs += (environment["PATH"] ?? "").split(separator: ":").map(String.init)
        let home = environment["HOME"] ?? NSHomeDirectory()
        dirs += ["/opt/homebrew/bin", "/usr/local/bin", "\(home)/.cargo/bin", "\(home)/.local/bin"]
        for d in dirs {
            let p = URL(fileURLWithPath: d).appending(path: "mcpgw").path
            if fm.isExecutableFile(atPath: p) { return URL(fileURLWithPath: p) }
        }
        return nil
    }

    /// The `PATH` a login shell sets up. Apps opened from Finder get only
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, which would leave the daemon unable
    /// to start servers such as `npx` or `uvx`.
    public static func loginShellPATH(environment: [String: String] = ProcessInfo.processInfo.environment) async -> String? {
        let shell = environment["SHELL"].flatMap { $0.isEmpty ? nil : $0 } ?? "/bin/zsh"
        let marker = "__MCPGW_PATH__"
        let r = try? await run(executable: URL(fileURLWithPath: shell),
                               args: ["-l", "-i", "-c", "printf '\(marker)%s\(marker)' \"$PATH\""],
                               environment: environment, timeout: 10)
        guard let out = r?.stdout, let a = out.range(of: marker),
              let b = out.range(of: marker, range: a.upperBound..<out.endIndex)
        else { return nil }
        let path = String(out[a.upperBound..<b.lowerBound])
        return path.isEmpty ? nil : path
    }

    /// `base` with `extra`'s entries appended where missing.
    public static func mergePATH(_ base: String?, _ extra: [String]) -> String {
        var seen = Set<String>()
        var out: [String] = []
        for d in (base ?? "").split(separator: ":").map(String.init) + extra where !d.isEmpty && seen.insert(d).inserted {
            out.append(d)
        }
        return out.joined(separator: ":")
    }

    /// Run `mcpgw [--config …] <args>`.
    public func run(_ args: [String], timeout: TimeInterval = 120) async throws -> CLIResult {
        let full = (config.map { ["--config", $0] } ?? []) + args
        return try await Self.run(executable: binary, args: full, environment: environment, timeout: timeout)
    }

    /// Run and return stdout, or throw with what it printed.
    @discardableResult
    public func check(_ args: [String], timeout: TimeInterval = 120) async throws -> String {
        let r = try await run(args, timeout: timeout)
        guard r.ok else {
            throw CLIError(args: args, result: r,
                           message: r.output.isEmpty ? "mcpgw \(args.first ?? "") exited with \(r.status)" : r.output)
        }
        return r.stdout
    }

    /// The daemon's state, without starting it.
    public func status() async throws -> DaemonStatus {
        let out = try await check(["status"], timeout: 30)
        guard let s = DaemonStatus.parse(out) else {
            throw CLIError(args: ["status"], result: nil, message: "can't read `mcpgw status`: \(out)")
        }
        return s
    }

    /// The loopback token (created if missing).
    public func token() async throws -> String {
        try await check(["token"], timeout: 30).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Start the daemon in the background (`mcpgw up`).
    public func up() async throws { try await check(["up"], timeout: 90) }

    /// Stop it (`mcpgw down`).
    public func down() async throws { try await check(["down"], timeout: 60) }

    /// An API client for the running daemon, or `nil` when it isn't running.
    public func connect() async throws -> APIClient? {
        guard case .running(_, let mcp, _) = try await status() else { return nil }
        var comps = URLComponents(url: mcp, resolvingAgainstBaseURL: false)!
        comps.path = ""
        return APIClient(base: comps.url!, token: try await token())
    }

    static func run(executable: URL, args: [String], environment: [String: String],
                    timeout: TimeInterval) async throws -> CLIResult {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<CLIResult, Error>) in
            DispatchQueue.global(qos: .userInitiated).async {
                let p = Process()
                p.executableURL = executable
                p.arguments = args
                p.environment = environment
                p.standardInput = FileHandle.nullDevice
                let out = Pipe(), err = Pipe()
                p.standardOutput = out
                p.standardError = err
                do {
                    try p.run()
                } catch {
                    cont.resume(throwing: CLIError(args: args, result: nil,
                                                   message: "can't run \(executable.path): \(error.localizedDescription)"))
                    return
                }
                let timer = DispatchWorkItem { if p.isRunning { p.terminate() } }
                DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timer)
                // read both pipes at once, so neither fills up and blocks the process
                let group = DispatchGroup()
                nonisolated(unsafe) var errData = Data()
                group.enter()
                DispatchQueue.global().async {
                    errData = err.fileHandleForReading.readDataToEndOfFile()
                    group.leave()
                }
                let outData = out.fileHandleForReading.readDataToEndOfFile()
                group.wait()
                p.waitUntilExit()
                timer.cancel()
                cont.resume(returning: CLIResult(status: p.terminationStatus,
                                                 stdout: String(decoding: outData, as: UTF8.self),
                                                 stderr: String(decoding: errData, as: UTF8.self)))
            }
        }
    }
}

extension MCPGW {
    /// `mcpgw doctor --json`. It exits non-zero when a check fails, so the
    /// report is read either way.
    public func doctor() async throws -> [DoctorCheck] {
        let r = try await run(["doctor", "--json"], timeout: 120)
        do {
            return try JSONDecoder().decode([DoctorCheck].self, from: Data(r.stdout.utf8))
        } catch {
            throw CLIError(args: ["doctor"], result: r, message: r.output.isEmpty ? "mcpgw doctor printed nothing" : r.output)
        }
    }
}

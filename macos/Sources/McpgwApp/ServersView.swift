import McpgwKit
import SwiftUI

/// Servers: global ones, which every client sees, and the ones projects
/// declare in their `.mcpgw.toml`, one row per name with the projects
/// that use it.
struct ServersView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: ServerRow.ID?
    @State private var adding = false
    @State private var removing: Server?

    var body: some View {
        let rows = model.serverRows
        HStack(spacing: 0) {
            Table(rows, selection: $selection) {
                TableColumn("Server") { r in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(r.name).bold()
                        Text(subtitle(r)).font(.caption).foregroundStyle(.secondary)
                            .lineLimit(1).truncationMode(.head)
                    }
                }
                .width(min: 120, ideal: 150)
                TableColumn("Scope") { r in
                    switch r {
                    case .global: StateBadge(text: "global").help("Every client and project sees it")
                    case .project(_, let uses):
                        StateBadge(text: "project")
                            .help(uses.map(\.project.name).joined(separator: ", "))
                        Text(uses.count == 1 ? uses[0].project.name : "\(uses.count) projects")
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                .width(min: 80, ideal: 90)
                TableColumn("State") { r in StateBadge(text: r.state) }
                    .width(min: 70, ideal: 80)
                TableColumn("Kind") { r in
                    switch r {
                    case .global(let s): Text(s.transport)
                    case .project(_, let uses): Text(uses.first?.server.kind ?? "")
                    }
                }
                .width(min: 45, ideal: 50)
                TableColumn("Tools") { r in
                    switch r {
                    case .global(let s): Text("\(s.tools)").monospacedDigit()
                    case .project(_, let uses): Text("\(uses.map(\.server.tools).max() ?? 0)").monospacedDigit()
                    }
                }
                .width(min: 34, ideal: 40)
            }
            .contextMenu(forSelectionType: ServerRow.ID.self) { ids in
                if case .global(let s)? = rows.first(where: { ids.contains($0.id) }) { actions(s) }
            }
            .alternatingRowBackgrounds(.disabled)
            .frame(minWidth: 400, maxWidth: .infinity)
            .overlay {
                if rows.isEmpty {
                    ContentUnavailableView {
                        Label("No servers", systemImage: "server.rack")
                    } description: {
                        Text("Add one here, with `mcpgw add`, or import a client's servers with `mcpgw client import`. Servers a project declares in its `.mcpgw.toml` show here too.")
                    } actions: {
                        Button("Add Server…") { adding = true }
                    }
                }
            }

            Divider()
            Group {
                switch rows.first(where: { $0.id == selection }) {
                case .global(let s)?:
                    ServerDetail(server: s, remove: { removing = s })
                case .project(let name, let uses)?:
                    ProjectServerDetail(name: name, uses: uses).id(name)
                case nil:
                    ContentUnavailableView("Select a server", systemImage: "sidebar.right")
                }
            }
            .frame(width: 420)
            .frame(maxHeight: .infinity)
        }
        .toolbar {
            Button { adding = true } label: { Label("Add Server", systemImage: "plus") }
                .help("Add a global server")
        }
        .sheet(isPresented: $adding) { AddServerSheet() }
        .confirmationDialog("Remove \(removing?.name ?? "")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } })) {
            Button("Remove", role: .destructive) {
                if let s = removing { Task { await model.perform { try await $0.removeServer(s.name) } } }
                removing = nil
            }
        } message: {
            Text("The server stops and its pinned tools are forgotten. Its secrets stay in the vault.")
        }
    }

    private func subtitle(_ r: ServerRow) -> String {
        switch r {
        case .global(let s): s.target
        case .project(_, let uses): String((uses.first?.server.runs ?? "").drop(while: { $0 != " " }).dropFirst())
        }
    }

    @ViewBuilder func actions(_ s: Server) -> some View {
        Button(s.enabled ? "Disable" : "Enable") {
            Task { await model.perform { try await $0.setEnabled(s.name, !s.enabled) } }
        }
        .disabled(s.fromConfig)
        Button("Restart") { Task { await model.perform { try await $0.restart(s.name) } } }
        if s.displayState == "quarantined" {
            Button("Accept New Tools") { Task { await model.perform { _ = try await $0.pin(s.name) } } }
        }
        Divider()
        Button("Remove…", role: .destructive) { removing = s }.disabled(s.fromConfig)
    }
}

/// A server that projects declare: the projects that use it, each with
/// its own process, state and pinned tools, and the details of the one
/// picked.
struct ProjectServerDetail: View {
    @Environment(AppModel.self) private var model
    var name: String
    var uses: [ProjectUse]
    @State private var picked: ProjectUse.ID?
    @State private var status: Server?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(name).font(.title2.bold())
                    StateBadge(text: "project")
                    Spacer()
                }
                Text("Declared in the .mcpgw.toml of \(uses.count == 1 ? "1 project" : "\(uses.count) projects"). Clients see it only when started in one of them, as `\(name)__<tool>`; each project runs its own copy.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Associated projects").font(.headline).padding(.top, 4)
            }
            .padding([.horizontal, .top], 14)
            List(uses, selection: $picked) { u in
                HStack {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(u.project.name).bold()
                        Text(u.project.root).font(.caption).foregroundStyle(.secondary)
                            .lineLimit(1).truncationMode(.head)
                    }
                    Spacer()
                    if u.project.state != "trusted" { StateBadge(text: u.project.state) }
                    StateBadge(text: u.state)
                }
            }
            .frame(height: min(CGFloat(uses.count) * 44 + 12, 230))
            Divider()
            Group {
                if let s = status, let u = uses.first(where: { $0.id == picked }) {
                    ServerDetail(server: s, project: u.project.name, remove: {})
                } else if uses.first(where: { $0.id == picked })?.project.state != "trusted", picked != nil {
                    ContentUnavailableView("Not running", systemImage: "lock.shield",
                                           description: Text("The project isn't trusted, so its servers are off. Trust it in Projects."))
                } else {
                    ContentUnavailableView("Pick a project", systemImage: "folder",
                                           description: Text("Its copy's state, tools and logs show here."))
                }
            }
            .frame(maxHeight: .infinity)
        }
        .onAppear { if picked == nil { picked = uses.first?.id } }
        .task(id: picked) { await follow() }
    }

    /// The picked copy's status, refreshed while it's shown.
    private func follow() async {
        status = nil
        guard let id = picked, uses.first(where: { $0.id == id })?.project.state == "trusted" else { return }
        while !Task.isCancelled {
            if let api = model.api, let s = try? await api.serverStatus(id) { status = s }
            try? await Task.sleep(for: .seconds(2))
        }
    }
}

struct ServerDetail: View {
    @Environment(AppModel.self) private var model
    var server: Server
    /// The project whose `.mcpgw.toml` declares it; its file and trust
    /// decide whether it runs, so enable, disable and remove are hidden.
    var project: String? = nil
    var remove: () -> Void

    @State private var tab = 0
    @State private var tools: [Tool] = []
    @State private var logs: [String] = []
    @State private var spec: JSONValue?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text(project ?? server.name).font(project == nil ? .title2.bold() : .title3.bold())
                    StateBadge(text: server.displayState)
                    Spacer()
                }
                if let e = server.lastError, server.displayState != "disabled" {
                    Text(e).foregroundStyle(.red).textSelection(.enabled).font(.callout)
                }
                HStack {
                    if project == nil {
                        Button(server.enabled ? "Disable" : "Enable") {
                            Task { await model.perform { try await $0.setEnabled(server.name, !server.enabled) } }
                        }
                        .disabled(server.fromConfig)
                        .help(server.fromConfig ? "Defined in the config file; change it there" : "")
                    }
                    Button("Restart") { Task { await model.perform { try await $0.restart(server.name) } } }
                        .help("Clear failures and stop; it starts fresh on its next call")
                    if server.displayState == "quarantined" {
                        Button("Accept New Tools") { Task { await model.perform { _ = try await $0.pin(server.name) } } }
                            .buttonStyle(.borderedProminent)
                            .help("Its tools changed since they were pinned; accept the new ones")
                    }
                    Spacer()
                    if project == nil {
                        Button("Remove…", role: .destructive, action: remove).disabled(server.fromConfig)
                    }
                }
                Picker("", selection: $tab) {
                    Text("Details").tag(0)
                    Text("Tools (\(server.tools))").tag(1)
                    Text("Logs").tag(2)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }
            .padding(14)
            Divider()
            switch tab {
            case 0: details
            case 1: toolList
            default: logView
            }
        }
        .task(id: "\(server.name)-\(tab)") { await load() }
        .padding(.top, project == nil ? 0 : 2)
    }

    private var details: some View {
        Form {
            LabeledContent("Kind", value: server.transport)
            LabeledContent("Target") { Text(server.target).textSelection(.enabled) }
            if let a = server.alias { LabeledContent("Tool prefix", value: "\(a)__") }
            if let p = project {
                LabeledContent("Declared by", value: "\(p)'s .mcpgw.toml")
                LabeledContent("Internal name") { Text(server.name).font(.caption.monospaced()).textSelection(.enabled) }
            } else {
                LabeledContent("Source", value: server.source)
            }
            LabeledContent("Enabled", value: server.enabled ? "yes" : "no")
            LabeledContent("Stateful", value: server.stateful ? "yes" : "no")
            LabeledContent("Warm", value: server.warm ? "yes" : "no")
            if let p = server.pid { LabeledContent("Process", value: "pid \(p)") }
            if let c = server.container { LabeledContent("Container", value: "\(c.runtime) \(c.name) (\(c.image))") }
            if let v = server.protocolVersion { LabeledContent("Protocol", value: v) }
            if let r = server.restarts { LabeledContent("Restarts", value: "\(r)") }
            if let c = server.coldStarts { LabeledContent("Cold starts", value: "\(c)") }
            if let ms = server.lastStartMs { LabeledContent("Last start", value: "\(ms) ms") }
            if let p = server.pin { LabeledContent("Pin") { Text(p).font(.caption.monospaced()).textSelection(.enabled) } }
            if let spec = spec?["spec"] {
                SwiftUI.Section("Spec") {
                    Text(spec.pretty()).font(.caption.monospaced()).textSelection(.enabled)
                }
            }
        }
        .formStyle(.grouped)
    }

    private var toolList: some View {
        List(tools) { t in
            VStack(alignment: .leading, spacing: 2) {
                Text("\(server.prefix)__\(t.name)").font(.body.monospaced())
                if let d = t.description { Text(d).font(.caption).foregroundStyle(.secondary).lineLimit(3) }
            }
            .padding(.vertical, 2)
        }
        .overlay { if tools.isEmpty { ContentUnavailableView("No pinned tools", systemImage: "wrench") } }
    }

    private var logView: some View {
        ScrollView {
            Text(logs.isEmpty ? "No stderr output yet." : logs.joined(separator: "\n"))
                .font(.caption.monospaced())
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
        }
        .defaultScrollAnchor(.bottom)
        .toolbar {
            Button { Task { await load() } } label: { Label("Reload Logs", systemImage: "arrow.clockwise") }
        }
    }

    private func load() async {
        guard let api = model.api else { return }
        switch tab {
        case 0: spec = try? await api.server(server.name)
        case 1: tools = (try? await api.serverTools(server.name)) ?? []
        default:
            // follow the log while it's shown
            while !Task.isCancelled {
                logs = (try? await api.logs(server.name, lines: 500)) ?? logs
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }
}

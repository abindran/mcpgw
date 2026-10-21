import McpgwKit
import SwiftUI

struct ServersView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Server.ID?
    @State private var adding = false
    @State private var removing: Server?

    var body: some View {
        HStack(spacing: 0) {
            Table(model.servers, selection: $selection) {
                TableColumn("Server") { s in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(s.name).bold()
                        Text(s.target).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                TableColumn("State") { s in StateBadge(text: s.displayState) }
                    .width(min: 80, ideal: 95)
                TableColumn("Kind") { s in Text(s.transport) }
                    .width(min: 60, ideal: 75)
                TableColumn("Tools") { s in Text("\(s.tools)").monospacedDigit() }
                    .width(min: 40, ideal: 50)
                TableColumn("Source") { s in Text(s.source).foregroundStyle(.secondary) }
                    .width(min: 50, ideal: 60)
            }
            .contextMenu(forSelectionType: Server.ID.self) { ids in
                if let s = model.servers.first(where: { ids.contains($0.id) }) { actions(s) }
            }
            .alternatingRowBackgrounds(.disabled)
            .frame(minWidth: 380, maxWidth: .infinity)
            .overlay {
                if model.servers.isEmpty {
                    ContentUnavailableView {
                        Label("No servers", systemImage: "server.rack")
                    } description: {
                        Text("Add one here, with `mcpgw add`, or import a client's servers with `mcpgw client import`.")
                    } actions: {
                        Button("Add Server…") { adding = true }
                    }
                }
            }

            Divider()
            Group {
                if let s = model.servers.first(where: { $0.id == selection }) {
                    ServerDetail(server: s, remove: { removing = s })
                } else {
                    ContentUnavailableView("Select a server", systemImage: "sidebar.right")
                }
            }
            .frame(width: 400)
            .frame(maxHeight: .infinity)
        }
        .toolbar {
            Button { adding = true } label: { Label("Add Server", systemImage: "plus") }
                .help("Add a server")
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

struct ServerDetail: View {
    @Environment(AppModel.self) private var model
    var server: Server
    var remove: () -> Void

    @State private var tab = 0
    @State private var tools: [Tool] = []
    @State private var logs: [String] = []
    @State private var spec: JSONValue?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text(server.name).font(.title2.bold())
                    StateBadge(text: server.displayState)
                    Spacer()
                }
                if let e = server.lastError, server.displayState != "disabled" {
                    Text(e).foregroundStyle(.red).textSelection(.enabled).font(.callout)
                }
                HStack {
                    Button(server.enabled ? "Disable" : "Enable") {
                        Task { await model.perform { try await $0.setEnabled(server.name, !server.enabled) } }
                    }
                    .disabled(server.fromConfig)
                    .help(server.fromConfig ? "Defined in the config file; change it there" : "")
                    Button("Restart") { Task { await model.perform { try await $0.restart(server.name) } } }
                        .help("Clear failures and stop; it starts fresh on its next call")
                    if server.displayState == "quarantined" {
                        Button("Accept New Tools") { Task { await model.perform { _ = try await $0.pin(server.name) } } }
                            .buttonStyle(.borderedProminent)
                            .help("Its tools changed since they were pinned; accept the new ones")
                    }
                    Spacer()
                    Button("Remove…", role: .destructive, action: remove).disabled(server.fromConfig)
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
    }

    private var details: some View {
        Form {
            LabeledContent("Kind", value: server.transport)
            LabeledContent("Target") { Text(server.target).textSelection(.enabled) }
            if let a = server.alias { LabeledContent("Tool prefix", value: "\(a)__") }
            LabeledContent("Source", value: server.source)
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

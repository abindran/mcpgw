import McpgwKit
import SwiftUI

/// Every public tool, with an inspector to call one (as the web UI's
/// inspector does: policy, approvals and audit apply). The scope picks
/// whose tools: the default endpoint's (global servers), or a project's
/// endpoint's (its servers with the global ones), and calls go through
/// that endpoint.
struct ToolsView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Tool.ID?
    @State private var search = ""
    /// The project whose endpoint is shown; empty for the global one.
    @AppStorage("toolsScope") private var scope = ""
    @State private var projectTools: [Tool] = []
    @State private var loadError: String?

    /// The projects to pick from: trusted ones, whose servers run.
    private var projects: [Project] {
        model.projects.filter { $0.state == "trusted" }.sorted { $0.name < $1.name }
    }

    private var project: Project? { scope.isEmpty ? nil : projects.first { $0.id == scope } }

    private var tools: [Tool] { project == nil ? model.tools : projectTools }

    private var shown: [Tool] {
        search.isEmpty ? tools : tools.filter {
            $0.name.localizedCaseInsensitiveContains(search) || ($0.description ?? "").localizedCaseInsensitiveContains(search)
        }
    }

    var body: some View {
        HStack(spacing: 0) {
            List(shown, selection: $selection) { t in
                VStack(alignment: .leading, spacing: 2) {
                    Text(t.name).font(.body.monospaced())
                    if let d = t.description { Text(d).font(.caption).foregroundStyle(.secondary).lineLimit(2) }
                }
                .padding(.vertical, 2)
            }
            .searchable(text: $search, placement: .toolbar, prompt: "Filter tools")
            .frame(width: 340)
            .overlay {
                if let loadError {
                    ContentUnavailableView("Can't list its tools", systemImage: "exclamationmark.triangle",
                                           description: Text(loadError))
                } else if tools.isEmpty {
                    ContentUnavailableView("No tools", systemImage: "wrench.and.screwdriver", description: Text(emptyText))
                }
            }

            Divider()
            Group {
                if let t = tools.first(where: { $0.id == selection }) {
                    InspectorPane(tool: t, project: project).id("\(scope)/\(t.id)")
                } else {
                    ContentUnavailableView("Select a tool to call it", systemImage: "play.rectangle")
                }
            }
            .frame(minWidth: 360, maxWidth: .infinity, maxHeight: .infinity)
        }
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Picker("Scope", selection: $scope) {
                    Text("Global").tag("")
                    if !projects.isEmpty { Divider() }
                    ForEach(projects) { p in Text(p.name).tag(p.id) }
                }
                .pickerStyle(.menu)
                .fixedSize()
                .help("Whose tools: global servers', or a project's (its servers with the global ones), as a client started there sees them")
            }
        }
        .onChange(of: scope) { selection = nil }
        // again once the projects load, so a remembered pick resolves
        .task(id: "\(scope)|\(project != nil)") { await follow() }
    }

    private var emptyText: String {
        if let p = project { return "\(p.name) has no tools yet: its servers' tools appear once they're pinned." }
        return projects.isEmpty
            ? "Tools appear when a server is added and its tools are pinned."
            : "No global server has tools. Project servers' tools are listed under their project: pick one above."
    }

    /// A project's tools, refreshed every two seconds while it's picked.
    private func follow() async {
        projectTools = []
        loadError = nil
        // a project that's gone (forgotten, untrusted) falls back to global
        if !scope.isEmpty, project == nil, !model.projects.isEmpty { scope = ""; return }
        guard let id = project?.id else { return }
        while !Task.isCancelled {
            if let api = model.api {
                do {
                    let t = try await api.tools(project: id)
                    if t != projectTools { projectTools = t }
                    loadError = nil
                } catch {
                    loadError = error.localizedDescription
                }
            }
            try? await Task.sleep(for: .seconds(2))
        }
    }
}

struct InspectorPane: View {
    @Environment(AppModel.self) private var model
    var tool: Tool
    /// Calls go through this project's endpoint, as a client there would.
    var project: Project? = nil

    @State private var arguments = ""
    @State private var result: CallResult?
    @State private var failure: String?
    @State private var calling = false
    @State private var elapsed: TimeInterval?
    @State private var showSchema = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(tool.name).font(.title3.monospaced().bold()).textSelection(.enabled)
            if let p = project {
                Label("Called as in \(p.name) (\(p.root))", systemImage: "folder")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            if let d = tool.description {
                Text(d).foregroundStyle(.secondary).textSelection(.enabled)
            }
            DisclosureGroup("Input schema", isExpanded: $showSchema) {
                ScrollView {
                    Text(tool.inputSchema?.pretty() ?? "{}")
                        .font(.caption.monospaced()).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 160)
            }
            Text("Arguments (JSON)").font(.headline)
            TextEditor(text: $arguments)
                .font(.body.monospaced())
                .frame(minHeight: 90, maxHeight: 180)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.quaternary))
            HStack {
                Button { Task { await call() } } label: { Label("Call", systemImage: "play.fill") }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(calling)
                Button("Reset") { arguments = sampleArguments(tool.inputSchema).pretty() }
                if calling {
                    ProgressView().controlSize(.small)
                    Text("Calling… it may be waiting for approval").font(.caption).foregroundStyle(.secondary)
                } else if let elapsed {
                    Text(String(format: "%.0f ms", elapsed * 1000)).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
            }
            if let failure {
                Text(failure).foregroundStyle(.red).textSelection(.enabled)
            }
            if let result {
                HStack {
                    Text(result.isError ? "Tool error" : "Result").font(.headline)
                        .foregroundStyle(result.isError ? .red : .primary)
                    Spacer()
                    Button { copyToPasteboard(result.text) } label: { Image(systemName: "doc.on.doc") }
                        .buttonStyle(.borderless).help("Copy")
                }
                ScrollView {
                    Text(result.text)
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 6))
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .onAppear { arguments = sampleArguments(tool.inputSchema).pretty() }
    }

    private func call() async {
        guard let api = model.api else { return }
        let args: JSONValue
        do {
            args = try JSONValue.parse(arguments.isEmpty ? "{}" : arguments)
        } catch {
            failure = "The arguments aren't valid JSON."
            return
        }
        guard args.object != nil else {
            failure = "The arguments must be a JSON object."
            return
        }
        failure = nil
        calling = true
        let t0 = Date()
        defer { calling = false; elapsed = Date().timeIntervalSince(t0) }
        do {
            result = try await api.callTool(tool.name, arguments: args, project: project?.id)
        } catch {
            result = nil
            failure = error.localizedDescription
        }
        await model.refresh()
    }
}

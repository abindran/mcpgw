import AppKit
import McpgwKit
import SwiftUI

/// Projects: repositories whose `.mcpgw.toml` lists servers for clients
/// started there. A project's servers stay off until its file is reviewed
/// and trusted here (or with `mcpgw project trust`); any edit turns them
/// off again.
struct ProjectsView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Project.ID?
    @State private var picking = false

    var body: some View {
        HStack(spacing: 0) {
            List(model.projects, selection: $selection) { p in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text(p.name).bold()
                        StateBadge(text: p.state)
                    }
                    Text(p.root).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.head)
                }
                .padding(.vertical, 2)
            }
            .frame(width: 300)
            .overlay {
                if model.projects.isEmpty {
                    ContentUnavailableView {
                        Label("No projects", systemImage: "folder.badge.gearshape")
                    } description: {
                        Text("A `.mcpgw.toml` in a repository lists servers for that project. Projects appear here when a client starts `mcpgw connect` in one, or when you add one.")
                    } actions: {
                        Button("Add Project…") { picking = true }
                    }
                }
            }
            Divider()
            Group {
                if let p = model.projects.first(where: { $0.id == selection }) {
                    ProjectDetail(project: p).id(p.id)
                } else {
                    ContentUnavailableView("Select a project", systemImage: "sidebar.right")
                }
            }
            .frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
        }
        .toolbar {
            Button { picking = true } label: { Label("Add Project", systemImage: "plus") }
                .help("Add a project folder")
        }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { r in
            if case .success(let url) = r { Task { await add(url) } }
        }
        .task { await model.refreshProjects() }
    }

    /// Register the folder; offer to write a `.mcpgw.toml` when it has none.
    private func add(_ url: URL) async {
        let file = url.appending(path: ".mcpgw.toml")
        if !FileManager.default.fileExists(atPath: file.path) {
            let alert = NSAlert()
            alert.messageText = "\(url.lastPathComponent) has no .mcpgw.toml"
            alert.informativeText = "Create one from the template? It lists no servers until you add some."
            alert.addButton(withTitle: "Create")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { return }
            do {
                _ = try await model.initProject(at: url)
            } catch {
                model.notice = error.localizedDescription
                return
            }
        }
        let path = url.resolvingSymlinksInPath().path
        var registered: Project?
        await model.perform { registered = try await $0.registerProject(path: path) }
        await model.refreshProjects()
        if let registered { selection = registered.id }
    }
}

struct ProjectDetail: View {
    @Environment(AppModel.self) private var model
    var project: Project
    @State private var reviewing = false
    @State private var forgetting = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text(project.name).font(.title2.bold())
                    StateBadge(text: project.state)
                    Spacer()
                }
                Text(status).foregroundStyle(project.needsTrust ? .orange : .secondary)
                if let e = project.error {
                    Text(e).font(.callout.monospaced()).foregroundStyle(.red).textSelection(.enabled)
                }
                HStack {
                    if project.state != "trusted" && project.sha256 != nil {
                        Button("Review and Trust…") { reviewing = true }
                            .buttonStyle(.borderedProminent)
                            .disabled(project.servers.isEmpty && project.state == "untrusted")
                    }
                    if project.state == "trusted" {
                        Button("Untrust") { Task { await model.perform { _ = try await $0.untrustProject(project.id) } } }
                            .help("Turn its servers off")
                    }
                    Button("Open File") {
                        NSWorkspace.shared.open(URL(fileURLWithPath: project.root).appending(path: ".mcpgw.toml"))
                    }
                    Button("Show in Finder") {
                        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: project.root)])
                    }
                    Spacer()
                    Button("Forget…", role: .destructive) { forgetting = true }
                }
            }
            .padding(14)
            Divider()
            Form {
                LabeledContent("Folder") { Text(project.root).textSelection(.enabled) }
                LabeledContent("Endpoint") {
                    HStack {
                        Text(project.url).font(.body.monospaced()).textSelection(.enabled)
                        Button { copyToPasteboard(project.url) } label: { Image(systemName: "doc.on.doc") }
                            .buttonStyle(.borderless)
                    }
                }
                if let s = project.sha256 {
                    LabeledContent("File") { Text("sha256 \(s.prefix(12))").font(.caption.monospaced()) }
                }
                SwiftUI.Section("Servers (\(project.servers.count))") {
                    if project.servers.isEmpty {
                        Text("None yet: add [[server]] entries to .mcpgw.toml.").foregroundStyle(.secondary)
                    }
                    ForEach(project.servers) { s in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack {
                                Text(s.name).bold()
                                if let st = s.state { StateBadge(text: st) }
                                if s.tools > 0 { Text("\(s.tools) tools").font(.caption).foregroundStyle(.secondary) }
                            }
                            Text(s.runs).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                    }
                }
                SwiftUI.Section {
                    Text("Clients started in this folder with `mcpgw connect` see these servers with your global ones; on a name clash the project's win. `mcpgw project init --client claude-code` (or cursor, vscode) adds the entry to the project's client config.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
        }
        .sheet(isPresented: $reviewing) { TrustSheet(project: project) }
        .confirmationDialog("Forget \(project.name)?", isPresented: $forgetting) {
            Button("Forget", role: .destructive) {
                Task { await model.perform { try await $0.forgetProject(project.id) } }
            }
        } message: {
            Text("Its servers stop and its trust is removed. It comes back untrusted the next time a client starts in it.")
        }
    }

    private var status: String {
        switch project.state {
        case "trusted": "Trusted: its servers run for clients in this folder."
        case "changed": "The file changed since it was trusted, so its servers are off. Review the new content to turn them on."
        case "untrusted": project.servers.isEmpty ? "No servers yet." : "Not trusted yet: its servers are off until you review and trust the file."
        case "invalid": "The file has errors, so its servers are off."
        case "missing": "The .mcpgw.toml is gone."
        default: project.state
        }
    }
}

/// Shows exactly what trusting the file will run, and trusts that content
/// only (the hash shown is sent along, so an edit in between is refused).
struct TrustSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    var project: Project
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Trust \(project.name)?", systemImage: "exclamationmark.shield")
                .font(.title3.bold())
            Text("Trusting runs these commands on this Mac, with the vault secrets listed, whenever a client in \(project.root) uses them. Only trust files you've read.")
                .fixedSize(horizontal: false, vertical: true)
            if project.state == "changed" {
                Text("This file changed since you last trusted it.").foregroundStyle(.orange)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(project.servers) { s in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(s.name).bold()
                            Text(s.runs).font(.caption.monospaced()).textSelection(.enabled)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
            }
            .frame(maxHeight: 240)
            .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 6))
            Text("File sha256 \(project.sha256?.prefix(16) ?? "")…").font(.caption.monospaced()).foregroundStyle(.secondary)
            HStack {
                Button("Open File") {
                    NSWorkspace.shared.open(URL(fileURLWithPath: project.root).appending(path: ".mcpgw.toml"))
                }
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Trust and Run") {
                    working = true
                    Task {
                        let ok = await model.perform { [project] in
                            _ = try await $0.trustProject(project.id, sha256: project.sha256)
                        }
                        working = false
                        if ok { dismiss() }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(working || project.sha256 == nil)
            }
        }
        .padding(18)
        .frame(width: 520)
    }
}

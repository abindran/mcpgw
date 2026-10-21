import McpgwKit
import SwiftUI

struct ProfilesView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Profile.ID?
    @State private var tools: [Tool] = []

    var body: some View {
        HStack(spacing: 0) {
            List(model.profiles, selection: $selection) { p in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text(p.name).bold()
                        Text(p.mode ?? "meta").font(.caption).foregroundStyle(.secondary)
                    }
                    Text(p.url).font(.caption.monospaced()).foregroundStyle(.secondary)
                }
            }
            .frame(width: 300)
            .overlay {
                if model.profiles.isEmpty {
                    ContentUnavailableView("No profiles", systemImage: "person.2.crop.square.stack",
                                           description: Text("Profiles give a client its own endpoint with a subset of servers and a policy. Define them under [profiles] in the config, or with PUT /api/v1/profiles."))
                }
            }
            Divider()
            Group {
                if let p = model.profiles.first(where: { $0.id == selection }) {
                    detail(p)
                } else {
                    ContentUnavailableView("Select a profile", systemImage: "sidebar.right")
                }
            }
            .frame(minWidth: 360, maxWidth: .infinity, maxHeight: .infinity)
        }
        .task(id: selection) {
            guard let api = model.api, let name = selection else { return }
            tools = (try? await api.tools(profile: name)) ?? []
        }
    }

    private func detail(_ p: Profile) -> some View {
        Form {
            LabeledContent("Endpoint") {
                HStack {
                    Text(p.url).font(.body.monospaced()).textSelection(.enabled)
                    Button { copyToPasteboard(p.url) } label: { Image(systemName: "doc.on.doc") }.buttonStyle(.borderless)
                }
            }
            LabeledContent("Mode", value: p.mode ?? "meta")
            LabeledContent("Servers", value: (p.servers ?? []).isEmpty ? "all" : (p.servers ?? []).joined(separator: ", "))
            LabeledContent("Source", value: p.source)
            if let pol = p.policy, pol != .object([:]) {
                SwiftUI.Section("Policy") {
                    Text(pol.pretty()).font(.caption.monospaced()).textSelection(.enabled)
                }
            }
            SwiftUI.Section("Tools it sees (\(tools.count))") {
                ForEach(tools) { t in Text(t.name).font(.caption.monospaced()) }
            }
            if !p.fromConfig {
                Button("Delete Profile", role: .destructive) {
                    Task { await model.perform { try await $0.removeProfile(p.name) } }
                }
            }
        }
        .formStyle(.grouped)
    }
}

struct SecretsView: View {
    @Environment(AppModel.self) private var model
    @State private var editing: SecretEdit?
    @State private var removing: SecretMeta?

    var body: some View {
        Table(model.secrets) {
            TableColumn("Name") { s in Text(s.name).font(.body.monospaced()) }
            TableColumn("Description") { s in Text(s.description ?? "").foregroundStyle(.secondary) }
            TableColumn("Updated") { s in Text(when(unix: s.updatedAt)) }.width(min: 90, ideal: 140)
            TableColumn("Last used") { s in Text(s.lastUsedAt.map { when(unix: $0) } ?? "never") }.width(min: 90, ideal: 140)
            TableColumn("") { s in
                HStack {
                    Button("Change…") { editing = SecretEdit(name: s.name, description: s.description ?? "", isNew: false) }
                    Button("Delete…", role: .destructive) { removing = s }
                }
                .controlSize(.small)
            }
            .width(min: 140, ideal: 150)
        }
        .alternatingRowBackgrounds(.disabled)
        .overlay {
            if model.secrets.isEmpty {
                ContentUnavailableView("No secrets", systemImage: "key",
                                       description: Text("Secrets live in the encrypted vault. Servers get them as env vars, headers or files; values are redacted from results and logs."))
            }
        }
        .toolbar {
            Button { editing = SecretEdit(name: "", description: "", isNew: true) } label: { Label("Add Secret", systemImage: "plus") }
        }
        .sheet(item: $editing) { e in SecretSheet(edit: e) }
        .confirmationDialog("Delete \(removing?.name ?? "")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } })) {
            Button("Delete", role: .destructive) {
                if let s = removing { Task { await model.perform { try await $0.removeSecret(s.name) } } }
                removing = nil
            }
        } message: {
            Text("Servers that use it will fail to start until it's set again.")
        }
    }
}

struct SecretEdit: Identifiable {
    var name: String
    var description: String
    var isNew: Bool
    var id: String { isNew ? "+new" : name }
}

struct SecretSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State var edit: SecretEdit
    @State private var value = ""
    @State private var saving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(edit.isNew ? "Add Secret" : "Change \(edit.name)").font(.headline)
            Form {
                if edit.isNew { TextField("Name", text: $edit.name, prompt: Text("github_token")) }
                SecureField("Value", text: $value)
                TextField("Description", text: $edit.description, prompt: Text("optional"))
            }
            Text("The value goes straight to the vault; it can't be read back.").font(.caption).foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save") {
                    saving = true
                    Task {
                        if await model.perform({ [edit, value] in
                            try await $0.setSecret(edit.name, value: value, description: edit.description)
                        }) {
                            dismiss()
                        }
                        saving = false
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(saving || value.isEmpty || edit.name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(18)
        .frame(width: 420)
    }
}

struct ClientsView: View {
    @Environment(AppModel.self) private var model
    @State private var profile = ""
    @State private var output: [String: (ok: Bool, text: String)] = [:]
    @State private var running: String?

    private let clients = [
        ("claude-desktop", "Claude Desktop", "bubble.left.and.text.bubble.right"),
        ("claude-code", "Claude Code", "terminal"),
        ("cursor", "Cursor", "cursorarrow.rays"),
        ("vscode", "VS Code", "chevron.left.forwardslash.chevron.right"),
    ]

    var body: some View {
        Form {
            SwiftUI.Section {
                Picker("Profile", selection: $profile) {
                    Text("Default endpoint").tag("")
                    ForEach(model.profiles) { Text($0.name).tag($0.name) }
                }
            } footer: {
                Text("Adds an mcpgw entry to the client's config (`mcpgw client install`), keeping its other servers. The entry runs this app's bundled mcpgw, so keep the app where it is (e.g. /Applications).")
                    .font(.caption).foregroundStyle(.secondary)
            }
            SwiftUI.Section("Clients") {
                ForEach(clients, id: \.0) { id, name, icon in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Label(name, systemImage: icon)
                            Spacer()
                            if running == id { ProgressView().controlSize(.small) }
                            Button("Connect") { Task { await install(id) } }
                                .disabled(running != nil || model.mcpgw == nil)
                        }
                        if let o = output[id] {
                            Text(o.text).font(.caption.monospaced()).foregroundStyle(o.ok ? Color.secondary : Color.red)
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            SwiftUI.Section("Command line") {
                HStack {
                    Text("Link ~/.local/bin/mcpgw to the bundled binary, so terminals use the same version.")
                    Spacer()
                    Button("Install") { output["cli"] = (true, model.installCommandLineTool()) }
                }
                if let o = output["cli"] { Text(o.text).font(.caption).foregroundStyle(.secondary).textSelection(.enabled) }
            }
        }
        .formStyle(.grouped)
    }

    private func install(_ id: String) async {
        running = id
        defer { running = nil }
        do {
            output[id] = (true, try await model.installClient(id, profile: profile.isEmpty ? nil : profile))
        } catch {
            output[id] = (false, error.localizedDescription)
        }
    }
}

struct DoctorView: View {
    @Environment(AppModel.self) private var model
    @State private var checks: [DoctorCheck] = []
    @State private var running = false
    @State private var failure: String?

    private let order = ["fail": 0, "warn": 1, "info": 2, "ok": 3]

    var body: some View {
        List {
            if let failure { Text(failure).foregroundStyle(.red).textSelection(.enabled) }
            ForEach(Array(checks.sorted { (order[$0.level] ?? 9) < (order[$1.level] ?? 9) }.enumerated()), id: \.offset) { _, c in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    StateBadge(text: c.level).frame(width: 46)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(c.subject).bold()
                        Text(markdown(c.message)).textSelection(.enabled)
                        if let f = c.fix {
                            Text(markdown(f)).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                    }
                }
                .padding(.vertical, 3)
            }
        }
        .overlay { if running && checks.isEmpty { ProgressView("Running checks…") } }
        .toolbar {
            Button { Task { await run() } } label: { Label("Run Again", systemImage: "arrow.clockwise") }
                .disabled(running)
        }
        .task { await run() }
    }

    private func run() async {
        running = true
        defer { running = false }
        do {
            checks = try await model.doctor()
            failure = nil
        } catch {
            failure = error.localizedDescription
        }
    }
}

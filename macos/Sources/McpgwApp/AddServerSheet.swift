import McpgwKit
import SwiftUI
import UniformTypeIdentifiers

struct AddServerSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var form = AddServer()
    @State private var running = false
    @State private var output: String?
    @State private var failed = false
    @State private var picking = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Picker("Kind", selection: $form.kind) {
                    ForEach(AddServer.Kind.allCases) { Text($0.label).tag($0) }
                }
                TextField(form.kind == .registry ? "Registry entry" : "Name", text: $form.name,
                          prompt: Text(form.kind == .registry ? "io.github.github/github-mcp-server" : "github"))
                if form.kind != .registry {
                    HStack {
                        TextField(targetLabel, text: $form.target, prompt: Text(targetPrompt))
                        if form.kind == .wasm || form.kind == .openapi {
                            Button("Choose…") { picking = true }
                        }
                    }
                }
                if form.kind == .command || form.kind == .image {
                    TextField("Arguments", text: $form.arguments, prompt: Text(form.kind == .command ? "-y @modelcontextprotocol/server-filesystem ~/Documents" : ""))
                }
                if form.kind == .command || form.kind == .url {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(form.kind == .command ? "Environment (KEY=VALUE per line)" : "Headers (NAME=VALUE per line)")
                        TextEditor(text: $form.pairs).font(.body.monospaced()).frame(height: 54)
                        if form.kind == .url {
                            Text("Values may use ${secret:<name>}, e.g. Authorization=Bearer ${secret:api_key}.")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                if form.kind == .command || form.kind == .image {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Secrets as environment (VAR=SECRET per line)")
                        TextEditor(text: $form.secrets).font(.body.monospaced()).frame(height: 40)
                    }
                }
                if form.kind != .registry {
                    TextField("Tool prefix", text: $form.alias, prompt: Text("optional; default: the name"))
                }
                Toggle("Keep it running when idle (stateful)", isOn: $form.stateful)
                Toggle("Keep it warm (start with the gateway)", isOn: $form.warm)
                Toggle("Add it disabled", isOn: $form.disabled)
            }
            .formStyle(.grouped)
            .disabled(running)

            if let output {
                ScrollView {
                    Text(output)
                        .font(.caption.monospaced())
                        .foregroundStyle(failed ? .red : .primary)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                }
                .frame(maxHeight: 120)
                .background(.quaternary.opacity(0.4))
            }

            HStack {
                if running {
                    ProgressView().controlSize(.small)
                    Text("Starting it to read and pin its tools…").foregroundStyle(.secondary)
                }
                Spacer()
                Button(output != nil && !failed ? "Done" : "Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Add") { Task { await add() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(running || (output != nil && !failed))
            }
            .padding(14)
        }
        .frame(width: 560)
        .fileImporter(isPresented: $picking, allowedContentTypes: form.kind == .wasm ? [UTType(filenameExtension: "wasm") ?? .data] : [.json, .yaml, .data]) { r in
            if case .success(let url) = r { form.target = url.path }
        }
    }

    private var targetLabel: String {
        switch form.kind {
        case .command: "Command"
        case .url: "URL"
        case .image: "Image"
        case .wasm: "Component"
        case .openapi: "Spec"
        case .registry: ""
        }
    }

    private var targetPrompt: String {
        switch form.kind {
        case .command: "npx"
        case .url: "https://api.githubcopilot.com/mcp/"
        case .image: "ghcr.io/github/github-mcp-server"
        case .wasm: "/path/to/tool.wasm"
        case .openapi: "https://petstore3.swagger.io/api/v3/openapi.json"
        case .registry: ""
        }
    }

    private func add() async {
        running = true
        defer { running = false }
        do {
            output = try await model.addServer(form)
            failed = false
        } catch {
            output = error.localizedDescription
            failed = true
        }
    }
}

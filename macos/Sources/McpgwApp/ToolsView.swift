import McpgwKit
import SwiftUI

/// Every public tool, with an inspector to call one (as the web UI's
/// inspector does: policy, approvals and audit apply).
struct ToolsView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Tool.ID?
    @State private var search = ""

    private var shown: [Tool] {
        search.isEmpty ? model.tools : model.tools.filter {
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
                if model.tools.isEmpty {
                    ContentUnavailableView("No tools", systemImage: "wrench.and.screwdriver",
                                           description: Text("Tools appear when a server is added and its tools are pinned."))
                }
            }

            Divider()
            Group {
                if let t = model.tools.first(where: { $0.id == selection }) {
                    InspectorPane(tool: t).id(t.id)
                } else {
                    ContentUnavailableView("Select a tool to call it", systemImage: "play.rectangle")
                }
            }
            .frame(minWidth: 360, maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

struct InspectorPane: View {
    @Environment(AppModel.self) private var model
    var tool: Tool

    @State private var arguments = ""
    @State private var result: CallResult?
    @State private var failure: String?
    @State private var calling = false
    @State private var elapsed: TimeInterval?
    @State private var showSchema = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(tool.name).font(.title3.monospaced().bold()).textSelection(.enabled)
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
            result = try await api.callTool(tool.name, arguments: args)
        } catch {
            result = nil
            failure = error.localizedDescription
        }
        await model.refresh()
    }
}

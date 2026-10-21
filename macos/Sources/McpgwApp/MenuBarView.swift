import McpgwKit
import SwiftUI

/// The menu bar popover: the daemon's state, calls waiting for approval,
/// and quick actions.
struct MenuBarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                StatusDot(phase: model.phase)
                Text("mcpgw").font(.headline)
                Text(model.phase.label).foregroundStyle(.secondary)
                Spacer()
                DaemonButton()
            }
            if let r = model.runtime {
                Text("\(r.servers.running) of \(r.servers.total) servers running · \(model.tools.count) tools")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if !model.pendingApprovals.isEmpty {
                Divider()
                Text("Waiting for approval").font(.subheadline.bold())
                ScrollView {
                    VStack(alignment: .leading) {
                        ForEach(model.pendingApprovals) { a in ApprovalRow(approval: a, compact: true) }
                    }
                }
                .frame(maxHeight: 300)
            }
            let troubled = model.servers.filter { ["failed", "quarantined"].contains($0.displayState) }
            if !troubled.isEmpty {
                Divider()
                ForEach(troubled) { s in
                    HStack {
                        StateBadge(text: s.displayState)
                        Text(s.name)
                        Spacer()
                    }
                }
            }
            Divider()
            HStack {
                Button("Open mcpgw") {
                    openWindow(id: "main")
                    NSApp.activate()
                }
                if model.connected {
                    Button("Web UI") { model.openWebUI() }
                }
                Spacer()
                Button("Quit") { NSApp.terminate(nil) }
            }
            .controlSize(.small)
        }
        .padding(12)
        .frame(width: 340)
    }
}

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @AppStorage(Pref.configPath) private var configPath = ""
    @AppStorage(Pref.autoStart) private var autoStart = true
    @AppStorage(Pref.notifyApprovals) private var notifyApprovals = true
    @AppStorage(Pref.stopOnQuit) private var stopOnQuit = false
    @State private var draft = ""

    var body: some View {
        Form {
            SwiftUI.Section {
                Toggle("Start the gateway when the app opens", isOn: $autoStart)
                Toggle("Stop the gateway when the app quits", isOn: $stopOnQuit)
                Toggle("Notify me when a call waits for approval", isOn: $notifyApprovals)
            } footer: {
                Text("The daemon keeps serving clients after the app quits unless you stop it.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            SwiftUI.Section {
                HStack {
                    TextField("Config file", text: $draft, prompt: Text("default: ~/.config/mcpgw/config.toml"))
                    Button("Apply") {
                        configPath = draft
                        Task { await model.reconnect(start: autoStart) }
                    }
                    .disabled(draft == configPath)
                }
                LabeledContent("Binary") {
                    Text(model.mcpgw?.binary.path ?? "not found").font(.caption.monospaced()).textSelection(.enabled)
                }
                if let v = model.version { LabeledContent("Version", value: v) }
            } footer: {
                Text("The app runs mcpgw with --config when this is set. Stop the gateway before switching configs.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
        .onAppear { draft = configPath }
    }
}

import AppKit
import McpgwKit
import SwiftUI

@main
struct McpgwApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var model = AppModel()

    var body: some Scene {
        Window("mcpgw", id: "main") {
            MainView()
                .environment(model)
                .frame(minWidth: 860, minHeight: 540)
                .onAppear { delegate.model = model }
        }
        .defaultSize(width: 1080, height: 700)
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Install Command-Line Tool…") {
                    let msg = model.installCommandLineTool()
                    let alert = NSAlert()
                    alert.messageText = "Command-line tool"
                    alert.informativeText = msg
                    alert.runModal()
                }
            }
            CommandMenu("Gateway") {
                Button("Start") { Task { await model.startDaemon() } }
                    .disabled(model.connected || model.busy)
                Button("Stop") { Task { await model.stopDaemon() } }
                    .disabled(!model.connected || model.busy)
                Button("Restart") { Task { await model.restartDaemon() } }
                    .disabled(!model.connected || model.busy)
                Divider()
                Button("Refresh") { Task { await model.refresh(); await model.refreshSlow() } }
                    .keyboardShortcut("r")
                    .disabled(!model.connected)
                Button("Open Web UI") { model.openWebUI() }
                    .disabled(!model.connected)
            }
        }

        MenuBarExtra {
            MenuBarView().environment(model)
        } label: {
            Image(systemName: model.pendingApprovals.isEmpty ? "point.3.connected.trianglepath.dotted"
                                                             : "exclamationmark.bubble.fill")
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView().environment(model)
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: AppModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // `swift run` starts a bare executable; make it a regular app
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let model else { return .terminateNow }
        Task { @MainActor in
            await model.quitting()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}

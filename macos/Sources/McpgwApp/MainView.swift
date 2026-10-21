import McpgwKit
import SwiftUI

enum Section: String, CaseIterable, Identifiable, Hashable {
    case overview, servers, projects, tools, approvals, activity, profiles, secrets, audit, clients, doctor

    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .servers: "Servers"
        case .projects: "Projects"
        case .tools: "Tools"
        case .approvals: "Approvals"
        case .activity: "Activity"
        case .profiles: "Profiles"
        case .secrets: "Secrets"
        case .audit: "Audit Log"
        case .clients: "Clients"
        case .doctor: "Doctor"
        }
    }

    var icon: String {
        switch self {
        case .overview: "gauge.with.dots.needle.33percent"
        case .servers: "server.rack"
        case .projects: "folder.badge.gearshape"
        case .tools: "wrench.and.screwdriver"
        case .approvals: "hand.raised"
        case .activity: "list.bullet.rectangle"
        case .profiles: "person.2.crop.square.stack"
        case .secrets: "key"
        case .audit: "checkmark.seal"
        case .clients: "app.connected.to.app.below.fill"
        case .doctor: "stethoscope"
        }
    }

    /// Sections that only make sense with a running daemon.
    var needsDaemon: Bool { ![.overview, .clients, .doctor].contains(self) }
}

struct MainView: View {
    @Environment(AppModel.self) private var model
    @AppStorage("section") private var section: Section = .overview

    var body: some View {
        NavigationSplitView {
            List(selection: Binding(get: { Optional(section) }, set: { if let s = $0 { section = s } })) {
                ForEach(Section.allCases) { s in
                    Label(s.title, systemImage: s.icon)
                        .badge(badge(s))
                        .tag(s)
                }
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 200)
            .safeAreaInset(edge: .bottom) { DaemonFooter().padding(10) }
        } detail: {
            VStack(spacing: 0) {
                if let n = model.notice {
                    NoticeBar(text: n) { model.notice = nil }
                }
                Group {
                    if section.needsDaemon && !model.connected {
                        NotRunningView()
                    } else {
                        detail
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationTitle(section.title)
        }
    }

    private func badge(_ s: Section) -> Int {
        switch s {
        case .approvals: model.pendingApprovals.count
        case .projects: model.projectsNeedingTrust.count
        default: 0
        }
    }

    @ViewBuilder private var detail: some View {
        switch section {
        case .overview: OverviewView()
        case .servers: ServersView()
        case .projects: ProjectsView()
        case .tools: ToolsView()
        case .approvals: ApprovalsView()
        case .activity: ActivityView()
        case .profiles: ProfilesView()
        case .secrets: SecretsView()
        case .audit: AuditView()
        case .clients: ClientsView()
        case .doctor: DoctorView()
        }
    }
}

/// The daemon's state and its start/stop button, under the sidebar.
struct DaemonFooter: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 8) {
            StatusDot(phase: model.phase)
            VStack(alignment: .leading, spacing: 1) {
                Text(model.phase.label).font(.callout.weight(.medium))
                if let r = model.runtime {
                    Text("\(r.servers.running)/\(r.servers.total) servers up")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            DaemonButton()
        }
    }
}

struct DaemonButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.busy {
            ProgressView().controlSize(.small)
        } else if model.connected {
            Button("Stop") { Task { await model.stopDaemon() } }
                .controlSize(.small)
                .help("Stop the daemon (mcpgw down)")
        } else if case .starting = model.phase {
            ProgressView().controlSize(.small)
        } else {
            Button("Start") { Task { await model.startDaemon() } }
                .controlSize(.small)
                .disabled(model.mcpgw == nil)
                .help("Start the daemon (mcpgw up)")
        }
    }
}

struct StatusDot: View {
    var phase: Phase

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 9, height: 9)
            .accessibilityLabel(phase.label)
    }

    private var color: Color {
        switch phase {
        case .connected: .green
        case .starting: .yellow
        case .stopped: .secondary
        case .broken: .red
        }
    }
}

struct NoticeBar: View {
    var text: String
    var dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
            Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            Button { dismiss() } label: { Image(systemName: "xmark") }
                .buttonStyle(.borderless)
                .help("Dismiss")
        }
        .padding(10)
        .background(.orange.opacity(0.12))
    }
}

struct NotRunningView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.phase {
        case .starting(let s):
            ProgressView(s)
        case .broken(let why):
            ContentUnavailableView {
                Label("mcpgw is unavailable", systemImage: "exclamationmark.octagon")
            } description: {
                Text(why).textSelection(.enabled)
            } actions: {
                Button("Try Again") { Task { await model.reconnect() } }
            }
        default:
            ContentUnavailableView {
                Label("The gateway is stopped", systemImage: "pause.circle")
            } description: {
                Text("Start the daemon to manage servers, tools and approvals.")
            } actions: {
                Button("Start Gateway") { Task { await model.startDaemon() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.busy || model.mcpgw == nil)
            }
        }
    }
}

/// A short relative or absolute time.
func when(_ d: Date) -> String {
    if abs(d.timeIntervalSinceNow) < 86_400 {
        return d.formatted(date: .omitted, time: .standard)
    }
    return d.formatted(date: .abbreviated, time: .shortened)
}

func when(unix secs: Int) -> String { when(Date(timeIntervalSince1970: Double(secs))) }

func duration(_ secs: Int) -> String {
    let f = DateComponentsFormatter()
    f.allowedUnits = secs >= 3600 ? [.day, .hour, .minute] : [.minute, .second]
    f.unitsStyle = .abbreviated
    f.maximumUnitCount = 2
    return f.string(from: TimeInterval(secs)) ?? "\(secs)s"
}

/// mcpgw's messages quote commands in backticks; show them as code.
func markdown(_ s: String) -> AttributedString {
    (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
}

func copyToPasteboard(_ s: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(s, forType: .string)
}

/// A server or call state as a coloured capsule, like the web UI's badges.
struct StateBadge: View {
    var text: String

    var body: some View {
        Text(text.replacingOccurrences(of: "_", with: " "))
            .font(.caption.weight(.medium))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch text {
        case "ready", "idle", "ok", "approved": .green
        case "global": .purple
        case "project": .teal
        case "busy", "info": .blue
        case "trusted": .green
        case "untrusted", "changed", "missing": .orange
        case "invalid": .red
        case "starting", "draining", "backoff", "pending", "tool_error", "warn": .orange
        case "failed", "quarantined", "error", "denied", "fail": .red
        default: .secondary
        }
    }
}

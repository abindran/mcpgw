import McpgwKit
import SwiftUI

struct OverviewView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                header
                if model.connected, let r = model.runtime {
                    HStack(spacing: 12) {
                        Stat(title: "Servers running", value: "\(r.servers.running) / \(r.servers.total)", icon: "server.rack")
                        Stat(title: "Tools", value: "\(model.tools.count)", icon: "wrench.and.screwdriver")
                        Stat(title: "Waiting for approval", value: "\(r.approvalsPending)", icon: "hand.raised",
                             tint: r.approvalsPending > 0 ? .orange : .secondary)
                        Stat(title: "Uptime", value: duration(r.uptimeSecs), icon: "clock")
                    }
                    endpoints(r)
                    attention
                } else {
                    NotRunningView().frame(maxWidth: .infinity, minHeight: 260)
                }
            }
            .padding(20)
            .frame(maxWidth: 900, alignment: .leading)
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 14) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 34))
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 3) {
                Text("mcpgw").font(.title.bold())
                HStack(spacing: 6) {
                    StatusDot(phase: model.phase)
                    Text(statusLine).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if model.connected {
                Button("Open Web UI") { model.openWebUI() }
                Button("Restart") { Task { await model.restartDaemon() } }.disabled(model.busy)
            }
            DaemonButton().controlSize(.regular)
        }
    }

    private var statusLine: String {
        var parts = [model.phase.label]
        if let r = model.runtime { parts.append("pid \(r.pid)") }
        if let v = model.version { parts.append(v) }
        if let b = model.mcpgw?.binary.path {
            parts.append(b.contains(".app/Contents/") ? "bundled" : b)
        }
        return parts.joined(separator: " · ")
    }

    private func endpoints(_ r: Runtime) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(r.endpoints, id: \.self) { e in
                    HStack {
                        Text(e).font(.body.monospaced()).textSelection(.enabled)
                        Spacer()
                        Button { copyToPasteboard(e) } label: { Image(systemName: "doc.on.doc") }
                            .buttonStyle(.borderless)
                            .help("Copy")
                    }
                }
                Text("Clients connect to these with the token from `mcpgw token`, or through `mcpgw connect` (Clients sets that up).")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .padding(6)
        } label: {
            Label("Endpoints", systemImage: "link")
        }
    }

    @ViewBuilder private var attention: some View {
        let troubled = model.servers.filter { ["failed", "quarantined", "backoff"].contains($0.displayState) }
        if !troubled.isEmpty || !model.pendingApprovals.isEmpty || !model.projectsNeedingTrust.isEmpty {
            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.pendingApprovals) { a in
                        HStack {
                            Image(systemName: "hand.raised.fill").foregroundStyle(.orange)
                            Text("\(a.tool) waits for approval").bold()
                            Text(a.endpoint).foregroundStyle(.secondary)
                            Spacer()
                            Button("Approve") { Task { await model.decide(a, approve: true) } }
                            Button("Deny") { Task { await model.decide(a, approve: false) } }
                        }
                    }
                    ForEach(model.projectsNeedingTrust) { p in
                        HStack {
                            Image(systemName: "folder.badge.gearshape").foregroundStyle(.orange)
                            Text("Project \(p.name)").bold()
                            Text(p.state == "changed" ? "changed since it was trusted" : "\(p.servers.count) server(s) wait to be trusted")
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text("Review in Projects").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    ForEach(troubled) { s in
                        HStack(alignment: .firstTextBaseline) {
                            StateBadge(text: s.displayState)
                            Text(s.name).bold()
                            Text(s.lastError ?? "").foregroundStyle(.secondary).lineLimit(2)
                            Spacer()
                            if s.displayState == "quarantined" {
                                Button("Accept New Tools") { Task { await model.perform { _ = try await $0.pin(s.name) } } }
                            }
                            Button("Restart") { Task { await model.perform { try await $0.restart(s.name) } } }
                        }
                    }
                }
                .padding(6)
            } label: {
                Label("Needs attention", systemImage: "exclamationmark.circle")
            }
        }
    }
}

struct Stat: View {
    var title: String
    var value: String
    var icon: String
    var tint: Color = .secondary

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(title, systemImage: icon).font(.caption).foregroundStyle(tint)
            Text(value).font(.title2.weight(.semibold)).monospacedDigit()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }
}

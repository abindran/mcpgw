import McpgwKit
import SwiftUI

struct ApprovalsView: View {
    @Environment(AppModel.self) private var model
    @State private var showDecided = false

    private var shown: [Approval] {
        showDecided ? model.approvals.sorted { $0.createdAt > $1.createdAt } : model.pendingApprovals
    }

    var body: some View {
        List {
            ForEach(shown) { a in ApprovalRow(approval: a) }
        }
        .overlay {
            if shown.isEmpty {
                ContentUnavailableView("Nothing is waiting", systemImage: "hand.thumbsup",
                                       description: Text("Calls to tools a policy marks `approve` wait here until you decide."))
            }
        }
        .toolbar {
            Toggle("Show decided", isOn: $showDecided).toggleStyle(.checkbox)
        }
    }
}

struct ApprovalRow: View {
    @Environment(AppModel.self) private var model
    var approval: Approval
    var compact = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(approval.tool).font(.body.monospaced().bold())
                if !approval.pending { StateBadge(text: approval.status) }
                Spacer()
                Text(when(unix: approval.createdAt)).font(.caption).foregroundStyle(.secondary)
            }
            Text("\(approval.endpoint)\(approval.client.map { " · \($0)" } ?? "")")
                .font(.caption).foregroundStyle(.secondary)
            Text(approval.arguments)
                .font(.caption.monospaced())
                .lineLimit(compact ? 3 : 12)
                .textSelection(.enabled)
                .padding(6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 5))
            if approval.pending {
                HStack {
                    Button("Approve") { Task { await model.decide(approval, approve: true) } }
                        .buttonStyle(.borderedProminent)
                    Button("Deny") { Task { await model.decide(approval, approve: false) } }
                }
                .controlSize(compact ? .small : .regular)
            }
        }
        .padding(.vertical, 4)
    }
}

struct ActivityView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Table(model.calls) {
            TableColumn("Time") { c in Text(when(c.date)).monospacedDigit() }
                .width(min: 80, ideal: 90)
            TableColumn("Tool") { c in Text(c.tool).font(.body.monospaced()) }
            TableColumn("Endpoint") { c in
                Text("\(c.endpoint)\(c.client.map { " · \($0)" } ?? "")").foregroundStyle(.secondary)
            }
            TableColumn("Outcome") { c in StateBadge(text: c.outcome) }
                .width(min: 70, ideal: 85)
            TableColumn("ms") { c in Text("\(c.durationMs)").monospacedDigit() }
                .width(min: 40, ideal: 60)
        }
        .alternatingRowBackgrounds(.disabled)
        .overlay {
            if model.calls.isEmpty {
                ContentUnavailableView("No calls yet", systemImage: "list.bullet.rectangle",
                                       description: Text("Tool calls through any endpoint show up here (names and outcomes only)."))
            }
        }
    }
}

struct AuditView: View {
    @Environment(AppModel.self) private var model
    @State private var entries: [AuditEntry] = []
    @State private var verify: AuditVerify?
    @State private var selection: AuditEntry.ID?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                if let v = verify {
                    if v.ok {
                        Label("Chain intact: \(v.entries ?? 0) entries", systemImage: "checkmark.seal.fill").foregroundStyle(.green)
                    } else {
                        Label("Chain broken at #\(v.brokenAt ?? 0): \(v.reason ?? "")", systemImage: "xmark.seal.fill").foregroundStyle(.red)
                    }
                }
                Spacer()
                Button("Verify") { Task { await load() } }
            }
            .padding(10)
            Divider()
            Table(entries.reversed(), selection: $selection) {
                TableColumn("#") { e in Text("\(e.seq)").monospacedDigit() }.width(min: 36, ideal: 50)
                TableColumn("Time") { e in Text(when(e.date)).monospacedDigit() }.width(min: 80, ideal: 140)
                TableColumn("Kind") { e in Text(e.kind).font(.body.monospaced()) }.width(min: 90, ideal: 120)
                TableColumn("Actor") { e in Text(e.actor) }
                TableColumn("Subject") { e in Text(e.subject).font(.body.monospaced()) }
            }
            .alternatingRowBackgrounds(.disabled)
            if let e = entries.first(where: { $0.id == selection }) {
                Divider()
                ScrollView {
                    Text(detail(e)).font(.caption.monospaced()).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(10)
                }
                .frame(height: 130)
            }
        }
        .task { await load() }
    }

    private func detail(_ e: AuditEntry) -> String {
        var lines = ["hash  \(e.hash)", "prev  \(e.prevHash)"]
        if let d = e.argsDigest { lines.append("args  sha256 \(d)") }
        if let d = e.detail { lines.append(d.pretty()) }
        return lines.joined(separator: "\n")
    }

    /// The newest 500 entries (the API pages oldest first).
    private func load() async {
        guard let api = model.api else { return }
        do {
            let v = try await api.verifyAudit()
            verify = v
            let total = v.entries ?? v.brokenAt ?? 0
            entries = try await api.audit(after: max(0, total - 500), limit: 500)
        } catch {
            model.notice = error.localizedDescription
        }
    }
}

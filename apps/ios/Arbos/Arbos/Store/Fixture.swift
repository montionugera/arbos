// Debug-only fixture: the Grok Bot reference conversation mapped onto Arbos
// concepts, so the simulator can be screenshotted without a relay. Enabled by
// `ARBOS_FIXTURE=1` in the launch environment. Not compiled into Release.

import Foundation

#if DEBUG
extension AppModel {
    func loadFixture() {
        isFixture = true
        let now = nowMs()
        let comms = Project(id: "fx-comms", name: "Comms", icon: "Co", color: "#1084FE", state: .awake, stateSince: now - 3_600_000,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0.4, spendCapUsd: 25)
        let affine = Project(id: "fx-affine", name: "Affine", icon: "Af", color: "#10B981", state: .awake, stateSince: now - 3_600_000,
                             stateDetail: nil, model: "anthropic/claude-sonnet-5.5", effort: .medium, spendTodayUsd: 1.2, spendCapUsd: 25)
        let hyper = Project(id: "fx-hyper", name: "Hyper", icon: "Hy", color: "#FF7A00", state: .awake, stateSince: now - 3_600_000,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let scout = Project(id: "fx-scout", name: "Talent Scout", icon: "Ts", color: "#8E5A3C", state: .awake, stateSince: now,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let expense = Project(id: "fx-expense", name: "Expense Manager", icon: "Em", color: "#FF2D9B", state: .awake, stateSince: now,
                              stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let invoices = Project(id: "fx-invoices", name: "Invoice Collector", icon: "Ic", color: "#8E8E93", state: .awake, stateSince: now,
                               stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        setFixture(projects: [comms, affine, hyper, scout, expense, invoices], now: now)
    }

    private func setFixture(projects list: [Project], now: Int64) {
        projects = list
        var allChats: [Id: [Chat]] = [:]
        var allMessages: [Id: [Message]] = [:]
        let previews: [Id: (String, Int64)] = [
            "fx-affine": ("Shortlist of 6 candidates ready.", now - 19 * 60_000),
            "fx-hyper": ("A/B copy variants ready to review.", now - 36 * 60_000),
            "fx-scout": ("Outreach drafts queued for approval.", now - 26 * 3_600_000),
            "fx-expense": ("Receipts coded — one needs a look.", now - 27 * 3_600_000),
            "fx-invoices": ("Pulled 9 invoices from vendor portals.", now - 3 * 86_400_000),
        ]
        for p in list {
            let chat = Chat(id: "\(p.id)-main", projectId: p.id, title: p.name, isPinned: true, unread: p.id == "fx-affine",
                            lastMessageAt: p.id == "fx-comms" ? now - 60_000 : (previews[p.id]?.1 ?? now))
            allChats[p.id] = [chat]
            if let (text, at) = previews[p.id] {
                allMessages[chat.id] = [assistant("\(p.id)-m1", chat.id, text, at: at)]
            }
            caughtUp.insert(chat.id)
        }
        let c = "fx-comms-main"
        let t0 = now - 14 * 60_000
        allMessages[c] = [
            user("c1", c, "That was really great what we set up with Alan. However, I think we need to be a little more careful about the way in which we type in Discord, not to write massive, verbose responses. Try to write in my style do things that make it seem like it's not an agent that's writing the response. so maybe you can rewrite messages that go through our Discord.", at: t0),
            assistant("c2", c, "Makes sense. I'll pull a sample of your own Discord messages to get your style, then set it so everything going through Discord gets rewritten to sound like you first.", at: t0 + 20_000),
            handoff("c3", c, name: "Affine", at: t0 + 40_000),
            assistant("c4", c, "From now on, anything I send on your Discord gets rewritten to sound like you first. That means short, casual one-liners like \"Can be done in a day\" or \"nice work with 10\", sometimes split into two or three quick messages. It also means no lists, no long blocks, and no \"— Arbos\" sign-off. I've told Affine to keep its answers to the key points too, and I'll stick to this style for anything else I send on your Discord.", at: t0 + 90_000),
            user("c5", c, "Hey", at: t0 + 120_000),
            streaming("c6", c, at: t0 + 125_000),
        ]
        // Affine's chat shows the card types: file, voice memo, question, stop.
        let a = "fx-affine-main"
        let a0 = now - 19 * 60_000
        allMessages[a] = [
            user("a1", a, "Pull the board deck numbers from Salesforce, Mercury and QuickBooks and send me the deck.", at: a0 - 15 * 60_000),
            assistant("a2", a, "Updated the numbers based on today's pull from Salesforce, Mercury, QuickBooks, and Hex.", at: a0 - 14 * 60_000),
            attachment("a3", a, name: "Q3 board deck v4.pdf", mime: "application/pdf", at: a0 - 13 * 60_000),
            user("a4", a, "Can you add a slide for headcount and pull from Rippling", at: a0 - 10 * 60_000),
            assistant("a5", a, "Pulled and added.", at: a0 - 9 * 60_000),
            attachment("a6", a, name: "Q3 board deck v5.pdf", mime: "application/pdf", at: a0 - 9 * 60_000 + 5_000),
            attachment("a7", a, name: "Walkthrough.m4a", mime: "audio/m4a", at: a0 - 8 * 60_000),
            question("a8", a, "Should I send the deck to the whole team or just the board list?", at: a0 - 2 * 60_000),
            stopped("a9", a, at: a0),
        ]
        chats = allChats
        messages = allMessages
        machine = Machine(state: .awake, since: now - 3_600_000, spec: "Daytona · 4 vCPU · 8 GB · 6 workspaces")
        link = .live
    }

    /// Sends in fixture mode append locally so the composer can be exercised,
    /// then a scripted reply streams in word by word.
    func fixtureAppend(chatId: Id, text: String, attachments: [Attachment]) {
        var list = messages[chatId] ?? []
        // Finish the fixture's stuck "working" turn so the demo reads cleanly.
        if let i = list.lastIndex(where: { $0.isStreaming }) {
            list[i].status = .done
            list[i].parts = [.text(id: "\(list[i].id)-t", text: "Got it.")]
        }
        var m = user(newId(), chatId, text, at: nowMs())
        m.parts = attachments.map { .attachment(id: newId(), attachment: $0) } + (text.isEmpty ? [] : m.parts)
        list.append(m)
        messages[chatId] = list
        Task { await fixtureReply(chatId: chatId) }
    }

    private func fixtureReply(chatId: Id) async {
        try? await Task.sleep(nanoseconds: 1_400_000_000)
        let id = newId()
        var m = Message(id: id, chatId: chatId, role: .assistant, sender: "agent", status: .streaming, turn: .reply, decision: nil,
                        subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: nowMs(),
                        completedAt: nil, pending: false, parts: [.status(id: "\(id)-s", code: .working, message: "Working")], hiddenCount: 0)
        messages[chatId, default: []].append(m)
        try? await Task.sleep(nanoseconds: 1_800_000_000)
        let words = "Done. Short replies from now on, and I'll keep anything that goes out on Discord to one or two casual lines in your voice.".split(separator: " ")
        var text = ""
        for w in words {
            text += (text.isEmpty ? "" : " ") + w
            m.parts = [.text(id: "\(id)-t", text: text)]
            if let i = messages[chatId]?.firstIndex(where: { $0.id == id }) { messages[chatId]?[i] = m }
            try? await Task.sleep(nanoseconds: 90_000_000)
        }
        m.status = .done
        m.completedAt = nowMs()
        if let i = messages[chatId]?.firstIndex(where: { $0.id == id }) { messages[chatId]?[i] = m }
    }

    private func user(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .user, sender: "user", status: .done, turn: nil, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.text(id: "\(id)-t", text: text)], hiddenCount: 0)
    }

    private func assistant(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: "anthropic/claude-haiku-5.5", createdAt: at, completedAt: at + 2_000,
                pending: false, parts: [.text(id: "\(id)-t", text: text)], hiddenCount: 0)
    }

    private func handoff(_ id: Id, _ chat: Id, name: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .spawn, decision: "new", subagentId: "sub-1",
                subagentName: name, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at + 5_000, pending: false,
                parts: [.status(id: "\(id)-s", code: .handoff, message: "Handed to subagent \(name)")], hiddenCount: 2)
    }

    private func attachment(_ id: Id, _ chat: Id, name: String, mime: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.attachment(id: "\(id)-a", attachment: Attachment(name: name, mime: mime, blobRef: "fixture-\(id)"))], hiddenCount: 0)
    }

    private func question(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.status(id: "\(id)-q", code: .askUser, message: text)], hiddenCount: 0)
    }

    private func stopped(_ id: Id, _ chat: Id, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .cancelled, turn: .reply, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.status(id: "\(id)-s", code: .stopped, message: "Stopped by the user")], hiddenCount: 0)
    }

    private func streaming(_ id: Id, _ chat: Id, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .streaming, turn: .reply, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: nil, pending: false,
                parts: [.status(id: "\(id)-s", code: .working, message: "Working")], hiddenCount: 0)
    }
}
#endif

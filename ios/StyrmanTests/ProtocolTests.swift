// JSON samples here are the exact shapes pinned by crates/styr-core/tests/wire_shape.rs
// and crates/styr-core/src/phone.rs: if one side changes, both test suites must.

import Foundation
import Testing
@testable import Styrman
import SwiftUI

private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

private func event(_ seq: UInt64, _ kind: String) -> String {
    #"{"session":"S","seq":\#(seq),"at_ms":1764000000000,"source":"hook","kind":\#(kind)}"#
}

@Suite struct ProtocolTests {
    @Test func toolCallDecodes() throws {
        let e = try decode(Event.self, event(7, #"""
        {"type":"tool_call","call":"toolu_1",
         "tool":{"name":"Bash","kind":"execute","title":"Run `cargo test`","input":{"command":"cargo test"}},
         "status":"completed","output":{"type":"text","text":"ok","truncated":false}}
        """#))
        guard case .toolCall(let call, let tool, let status, let output) = e.kind else { Issue.record("wrong kind"); return }
        #expect(call == "toolu_1")
        #expect(tool.kind == .execute)
        #expect(tool.input["command"]?.string == "cargo test")
        #expect(status == .completed)
        #expect(output?.displayText == "ok")
    }

    @Test func permissionShapesDecode() throws {
        let req = try decode(EventKind.self, #"""
        {"type":"permission_requested","request":"r1","call":"toolu_1",
         "tool":{"name":"Bash","kind":"execute","title":"Run `cargo test`","input":{"command":"cargo test"}},
         "reason":null,
         "options":[{"id":"allow","kind":"allow_once","label":"Allow once"},{"id":"deny","kind":"reject_once","label":"Deny"}],
         "expires_at_ms":1000}
        """#)
        guard case .permissionRequested(_, _, _, let options, let exp) = req else { Issue.record("wrong kind"); return }
        #expect(options.map(\.kind) == [.allowOnce, .rejectOnce])
        #expect(exp == 1000)

        let res = try decode(EventKind.self, #"""
        {"type":"permission_resolved","request":"r1","outcome":{"outcome":"selected","option":"allow","kind":"allow_once"},"by":{"by":"phone","device":"iphone"}}
        """#)
        #expect(res == .permissionResolved(request: "r1", outcome: .selected(option: "allow", kind: .allowOnce), by: .phone("iphone")))
        let term = try decode(EventKind.self, #"{"type":"permission_resolved","request":"r1","outcome":{"outcome":"cancelled"},"by":{"by":"terminal"}}"#)
        #expect(term == .permissionResolved(request: "r1", outcome: .cancelled, by: .terminal))
    }

    @Test func messagesAndUnknownTypes() throws {
        #expect(try decode(EventKind.self, #"{"type":"user_message","text":"hi","origin":{"from":"remote","device":"iphone"}}"#)
            == .userMessage(text: "hi", fromPhone: true))
        #expect(try decode(EventKind.self, #"{"type":"assistant_message","message":"m1","text":"Done.","mode":"final"}"#)
            == .assistantMessage(id: "m1", text: "Done.", delta: false))
        // Forward compatibility: a newer daemon's event type must not break decoding.
        #expect(try decode(EventKind.self, #"{"type":"compaction_update","whatever":1}"#) == .unknown(type: "compaction_update"))
        #expect(try decode(AgentKind.self, #""gemini""#) == .unknown)
    }

    @Test func serverMessagesDecode() throws {
        let hello = try decode(ServerMsg.self, #"{"type":"hello","protocol":1,"daemon_version":"0.1.0","device_id":"D","host":"mac"}"#)
        guard case .hello(let p, _, let d, let h, let w, let usageSupported, _) = hello else { Issue.record("not hello"); return }
        #expect((p, d, h) == (1, "D", "mac"))
        #expect(w.isEmpty, "warnings are optional")
        #expect(!usageSupported)
        let warned = try decode(ServerMsg.self, #"{"type":"hello","protocol":1,"daemon_version":"0.1.0","device_id":"D","host":"mac","warnings":["Claude Code 2.2.0 hasn't been checked"]}"#)
        guard case .hello(_, _, _, _, let w2, _, _) = warned else { Issue.record("not hello"); return }
        #expect(w2.count == 1)
        let fp = String(repeating: "ab", count: 32)
        let lanHello = try decode(ServerMsg.self, #"{"type":"hello","protocol":1,"daemon_version":"0.1.0","device_id":"D","host":"mac","lan_urls":["wss://192.168.1.20:4720/v1/ws","wss://8.8.8.8:4720/v1/ws","ws://10.0.0.2:4720/v1/ws"],"lan_cert":"\#(fp)"}"#)
        guard case .hello(_, _, _, _, _, _, let lan) = lanHello else { Issue.record("not hello"); return }
        #expect(lan?.urls.map(\.absoluteString) == ["wss://192.168.1.20:4720/v1/ws"], "only wss on private addresses")
        #expect(lan?.cert == fp)
        guard case .hello(_, _, _, _, _, _, let none) = hello else { return }
        #expect(none == nil)
        let sessions = try decode(ServerMsg.self, #"""
        {"type":"sessions","sessions":[{"id":"01S","agent":"claude","native_id":"abc","cwd":"/Users/dev/work/styrman",
         "title":null,"model":"claude-opus-5-5","shared":true,"status":"needs_approval",
         "created_ms":1,"updated_ms":2,"last_seq":3}]}
        """#)
        guard case .sessions(let list) = sessions else { Issue.record("not sessions"); return }
        #expect(list.first?.status == .needsApproval)
        #expect(list.first?.repoName == "styrman")
        #expect(try decode(ServerMsg.self, #"{"type":"session_removed","id":"S"}"#) != nil)
    }

    @Test func clientMessagesEncode() throws {
        let data = try JSONEncoder().encode(ClientMsg.subscribe(session: "S", sinceSeq: 4))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "subscribe")
        #expect(obj["session"] as? String == "S")
        #expect(obj["since_seq"] as? Int == 4)
    }

    /// Rust: ClientMsg::Command { session, command: Command::Rename { name } }
    @Test func renameEncodesAsRustReadsIt() throws {
        let data = try JSONEncoder().encode(ClientMsg.rename(session: "S", name: "Mouse figures"))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "command")
        let cmd = obj["command"] as! [String: Any]
        #expect(cmd["type"] as? String == "rename")
        #expect(cmd["name"] as? String == "Mouse figures")
        let json = #"{"id":"S","agent":"codex","native_id":"n","shared":true,"status":"waiting","created_ms":0,"updated_ms":0,"last_seq":0,"name":"Mouse figures"}"#
        let s = try JSONDecoder().decode(SessionSummary.self, from: Data(json.utf8))
        #expect(s.name == "Mouse figures")
    }

    @Test func notificationSettingsEncodeAsRustReadsThem() throws {
        // Rust: ClientMsg::SetNotifications { on, finished }
        let data = try JSONEncoder().encode(ClientMsg.setNotifications(on: true, finished: false))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "set_notifications")
        #expect(obj["on"] as? Bool == true)
        #expect(obj["finished"] as? Bool == false)
    }
}

@Suite struct CommandTests {
    @Test func resolvePermissionMatchesRust() throws {
        // Rust: ClientMsg::Command { session, command: Command::ResolvePermission { request, option } }
        let data = try JSONEncoder().encode(ClientMsg.resolvePermission(session: "S", request: "r1", option: "allow"))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "command")
        #expect(obj["session"] as? String == "S")
        let cmd = try #require(obj["command"] as? [String: Any])
        #expect(cmd["type"] as? String == "resolve_permission")
        #expect(cmd["request"] as? String == "r1")
        #expect(cmd["option"] as? String == "allow")
    }
}

@Suite struct PromptTests {
    @Test func sendPromptMatchesRust() throws {
        let data = try JSONEncoder().encode(ClientMsg.sendPrompt(session: "S", text: "run the tests"))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "command")
        let cmd = try #require(obj["command"] as? [String: Any])
        #expect(cmd["type"] as? String == "send_prompt")
        #expect(cmd["text"] as? String == "run the tests")
    }

    @Test func stopSharingMatchesRust() throws {
        let data = try JSONEncoder().encode(ClientMsg.stopSharing(session: "S"))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let cmd = try #require(obj["command"] as? [String: Any])
        #expect(cmd["type"] as? String == "set_sharing")
        #expect(cmd["shared"] as? Bool == false)
    }

    @Test func queuedDecodes() throws {
        let m = try decode(ServerMsg.self, #"{"type":"prompt_queued","session":"S","text":"hi","position":2}"#)
        guard case .promptQueued(let s, let t, let p) = m else { Issue.record("wrong"); return }
        #expect((s, t, p) == ("S", "hi", 2))
    }

    @Test @MainActor func pendingClearsEvenIfTheMacLostTrack() throws {
        let t = SessionTimeline(sessionId: "S")
        t.addPending("next is codex")
        t.markDelivered("next is codex")
        #expect(t.pending.first?.delivered == true)
        // After a daemon restart the typed prompt comes back as terminal input.
        t.apply([try decode(Event.self, event(1, #"{"type":"user_message","text":"next is codex","origin":{"from":"terminal"}}"#))])
        #expect(t.pending.isEmpty)
        let d = try decode(ServerMsg.self, #"{"type":"prompt_delivered","session":"S","text":"hi"}"#)
        guard case .promptDelivered(let s, let x) = d else { Issue.record("wrong"); return }
        #expect((s, x) == ("S", "hi"))
    }

    @Test @MainActor func pendingClearsWhenDelivered() throws {
        let t = SessionTimeline(sessionId: "S")
        t.addPending("hi")
        t.markQueued("hi")
        #expect(t.pending.first?.queued == true)
        t.apply([try decode(Event.self, event(1, #"{"type":"user_message","text":"hi","origin":{"from":"remote","device":"d"}}"#))])
        #expect(t.pending.isEmpty)
    }
}

@Suite struct PairingTests {
    @Test func parsesTheLinkStyrPairPrints() throws {
        // Same string as `link_is_url_safe` in crates/styr-core/src/phone.rs.
        let url = URL(string: "styrman://pair?v=1&url=ws%3A%2F%2F100.1.2.3%3A4719&code=AB%20CD&name=Mac%20mini")!
        let link = try #require(PairingLink(url: url))
        #expect(link.wsURL.absoluteString == "ws://100.1.2.3:4719")
        #expect(link.code == "AB CD")
        #expect(link.host == "Mac mini")
        #expect(link.httpBase.absoluteString == "http://100.1.2.3:4719")
    }

    @Test func rejectsForeignLinks() {
        #expect(PairingLink(url: URL(string: "https://example.com/pair?url=ws://x&code=1")!) == nil)
        #expect(PairingLink(url: URL(string: "styrman://pair?url=http%3A%2F%2Fx&code=1")!) == nil)
        #expect(PairingLink(url: URL(string: "styrman://pair?url=ws%3A%2F%2Fx")!) == nil)
    }

    @Test func manualEntryFillsDefaults() throws {
        let link = try #require(PairingLink(address: "100.126.198.54", code: "ABCDE-FGHJK"))
        #expect(link.wsURL.absoluteString == "ws://100.126.198.54:4719/v1/ws")
        #expect(PairingLink(address: "  ", code: "X") == nil)
    }
}

@Suite @MainActor struct TimelineTests {
    private func events(_ kinds: [String]) throws -> [Event] {
        try kinds.enumerated().map { i, k in try decode(Event.self, event(UInt64(i + 1), k)) }
    }

    @Test func foldsToolLifecycleAndPermissions() throws {
        let tool = #"{"name":"Bash","kind":"execute","title":"Run `ls`","input":{"command":"ls"}}"#
        let t = SessionTimeline(sessionId: "S")
        t.apply(try events([
            #"{"type":"sharing_changed","shared":true}"#,
            #"{"type":"user_message","text":"list files","origin":{"from":"terminal"}}"#,
            #"{"type":"tool_call","call":"c1","tool":\#(tool),"status":"pending","output":null}"#,
            #"{"type":"permission_requested","request":"r1","call":null,"tool":\#(tool),"reason":null,"options":[],"expires_at_ms":null}"#,
        ]))
        #expect(t.items.count == 4)
        #expect(t.openPermissions.count == 1)

        // Later events update in place; replays (seq ≤ lastSeq) are ignored.
        let more = try [
            #"{"type":"permission_resolved","request":"r1","outcome":{"outcome":"cancelled"},"by":{"by":"terminal"}}"#,
            #"{"type":"tool_call","call":"c1","tool":\#(tool),"status":"completed","output":{"type":"text","text":"a\nb","truncated":false}}"#,
            #"{"type":"assistant_message","message":"m","text":"Two files.","mode":"final"}"#,
        ].enumerated().map { i, k in try decode(Event.self, event(UInt64(5 + i), k)) }
        t.apply(more)
        t.apply(more)
        #expect(t.lastSeq == 7)
        #expect(t.items.count == 5)
        #expect(t.openPermissions.isEmpty)
        guard case .tool(_, let item) = t.items[2] else { Issue.record("tool row moved"); return }
        #expect(item.status == .completed)
        guard case .permission(_, let p) = t.items[3] else { Issue.record("permission row moved"); return }
        #expect(p.resolutionText == "Answered in the terminal")
    }

    @Test func streamedTextMerges() throws {
        let t = SessionTimeline(sessionId: "S")
        t.apply(try events([
            #"{"type":"assistant_message","message":"m","text":"Hel","mode":"delta"}"#,
            #"{"type":"assistant_message","message":"m","text":"lo","mode":"delta"}"#,
        ]))
        guard case .assistant(_, let text, _) = t.items.first else { Issue.record("no message"); return }
        #expect(text == "Hello")
        #expect(t.items.count == 1)
    }
}

@Suite struct DisplayTests {
    @Test func toolTitlesAreShortened() {
        let bash = ToolInfo(name: "Bash", kind: .execute, title: "Run `ls -la`", input: .object(["command": .string("ls -la")]), locations: nil)
        #expect(displayTitle(bash, cwd: "/r") == "ls -la")
        let edit = ToolInfo(name: "Edit", kind: .edit, title: "Edit /Users/dev/work/app/src/main.rs", input: .null, locations: nil)
        #expect(displayTitle(edit, cwd: "/Users/dev/work/app") == "Edit src/main.rs")
        #expect(displayTitle(edit, cwd: nil) == edit.title)
    }
}

@Suite struct NewSessionTests {
    @Test func startSessionMatchesRust() throws {
        let data = try JSONEncoder().encode(ClientMsg.startSession(agent: .codex, cwd: "/Users/me/work/x", prompt: "hi"))
        let obj = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        #expect(obj["type"] as? String == "start_session")
        #expect(obj["agent"] as? String == "codex")
        #expect(obj["cwd"] as? String == "/Users/me/work/x")
        #expect(obj["prompt"] as? String == "hi")
        let list = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ClientMsg.listFolders(path: nil))) as! [String: Any]
        #expect(list["type"] as? String == "list_folders")
        #expect(list["path"] == nil)
    }

    @Test func foldersDecode() throws {
        let json = """
        {"type": "folders", "listing": {"path": "/Users/me/work", "parent": "/Users/me", "home": "/Users/me",
         "entries": [{"name": "x", "path": "/Users/me/work/x", "is_repo": true}],
         "recent": ["/Users/me/work/x"], "agents": ["claude", "codex"]}}
        """
        guard case .folders(let l) = try decode(ServerMsg.self, json) else { Issue.record("not folders"); return }
        #expect(l.entries.first?.isRepo == true)
        #expect(l.agents == [.claude, .codex])
        #expect(l.display("/Users/me/work/x") == "~/work/x")
        #expect(l.display("/Users/me") == "~")
        guard case .sessionStarting(let s) = try decode(ServerMsg.self, #"{"type": "session_starting", "session": "S"}"#) else {
            Issue.record("not session_starting"); return
        }
        #expect(s == "S")
    }
}

/// One unreadable event must never break the stream (it would come back on every
/// reconnect and drop the connection again).
@Suite struct RobustDecodingTests {
    @Test func aBadEventInABatchIsSkippedAndTheRestKept() throws {
        let json = """
        {"type": "events", "session": "S", "events": [
          {"session": "S", "seq": 1, "at_ms": 0, "source": "hook", "kind": {"type": "user_message", "text": "hi", "origin": {"from": "terminal"}}},
          {"session": "S", "seq": 2, "at_ms": 0, "source": "hook", "kind": {"type": "permission_requested", "request": 42}},
          "not even an object",
          {"session": "S", "seq": 3, "at_ms": 0, "source": "hook", "kind": {"type": "notice", "text": "ok", "level": "info"}}
        ]}
        """
        let msg = try JSONDecoder().decode(ServerMsg.self, from: Data(json.utf8))
        guard case .events(_, let events) = msg else { Issue.record("not events"); return }
        #expect(events.map(\.seq) == [1, 2, 3], "the broken event still counts (as unknown), the non-event is skipped")
        if case .unknown(let type) = events[1].kind { #expect(type == "permission_requested") } else { Issue.record("expected unknown") }
    }

    @Test func aLiveEventWithAnOddValueStillDecodes() throws {
        let json = #"{"type": "event", "event": {"session": "S", "seq": 9, "at_ms": 1, "source": "pty", "kind": {"type": "permission_requested", "request": "r", "tool": {"name": "x", "kind": "execute", "title": "t", "input": null}, "options": [{"id": "o", "kind": "some_new_kind", "label": "Maybe"}]}}}"#
        let msg = try JSONDecoder().decode(ServerMsg.self, from: Data(json.utf8))
        guard case .event(let e) = msg else { Issue.record("not event"); return }
        #expect(e.seq == 9)
    }
}

@Suite @MainActor struct SendConfirmationTests {
    @Test func timeoutTargetsTheOriginalSendAndLateConfirmationClearsIt() {
        let t = SessionTimeline(sessionId: "S")
        let first = t.addPending("hello")
        let second = t.addPending("hello")
        t.markUnconfirmed(second)
        #expect(t.pending.map(\.unconfirmed) == [false, true])
        t.markQueued("hello")
        t.markUnconfirmed(first)
        #expect(t.pending.map(\.unconfirmed) == [false, true])
        t.markQueued("hello")
        #expect(t.pending.map(\.unconfirmed) == [false, false])
    }
}

@Suite struct OptionLabelTests {
    @Test func longCodexOptionsAreShortened() {
        let (h, c) = OptionLabel.split("Yes, and don't ask again for commands that start with `xcodebuild -project ios/Styrman.xcodeproj -quiet`")
        #expect(h == "Always allow commands starting with")
        #expect(c == "xcodebuild -project ios/Styrman.xcodeproj -quiet")
        #expect(OptionLabel.split("Yes, proceed").0 == "Allow once")
        #expect(OptionLabel.split("No, and tell Codex what to do differently").0 == "Deny and tell Codex what to do")
        #expect(OptionLabel.split("Allow once") == ("Allow once", nil), "Claude's labels stay as they are")
    }
}

@Suite struct FolderPickerTests {
    @Test func createFolderMatchesRust() throws {
        let obj = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ClientMsg.createFolder(parent: "/Users/me/work", name: "new app"))) as! [String: Any]
        #expect(obj["type"] as? String == "create_folder")
        #expect(obj["parent"] as? String == "/Users/me/work")
        #expect(obj["name"] as? String == "new app")
    }

    @Test func listingsCarryFilesAndBranch() throws {
        let json = #"{"type": "folders", "listing": {"path": "/Users/me/work", "parent": "/Users/me", "home": "/Users/me", "entries": [], "recent": [], "agents": ["codex"], "files": ["README.md"], "more_files": true, "git_branch": "main"}}"#
        guard case .folders(let l) = try JSONDecoder().decode(ServerMsg.self, from: Data(json.utf8)) else { Issue.record("not folders"); return }
        #expect(l.files == ["README.md"])
        #expect(l.moreFiles)
        #expect(l.gitBranch == "main")
        // An older daemon without these fields still decodes.
        let old = #"{"type": "folders", "listing": {"path": "/a", "parent": null, "home": "/a", "entries": [], "recent": [], "agents": []}}"#
        guard case .folders(let o) = try JSONDecoder().decode(ServerMsg.self, from: Data(old.utf8)) else { Issue.record("not folders"); return }
        #expect(o.files.isEmpty && o.gitBranch == nil)
    }
}

@Suite struct AgentMessageTests {
    @Test func modelsAndSwitching() throws {
        let msg = try decode(ServerMsg.self, #"{"type":"models","session":"S","models":[{"id":"sonnet","label":"Sonnet","efforts":["low","high"]},{"id":"gemini-4-neon-low","label":"Gemini 4 Neon (Low)"}]}"#)
        guard case .models(_, let list) = msg else { Issue.record("\(msg)"); return }
        #expect(list.map(\.id) == ["sonnet", "gemini-4-neon-low"])
        #expect(list[1].efforts.isEmpty)
        #expect(list[0].matches("claude-sonnet-5-5"), "an alias matches the full name")
        #expect(!list[0].matches("claude-opus-5-5"))
        #expect(list[1].matches("gemini-4-neon-low") && !list[1].matches("gemini-4-neon-lower"))
        #expect(ModelText.short("claude-haiku-5-5") == "Haiku 5.5")

        let sent = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ClientMsg.setModel(session: "S", model: "sonnet", effort: nil))) as! [String: Any]
        let cmd = sent["command"] as! [String: Any]
        #expect(cmd["type"] as? String == "set_model" && cmd["model"] as? String == "sonnet" && cmd["effort"] == nil)
    }

    @Test func commitRoundTrip() throws {
        let sent = String(data: try JSONEncoder().encode(ClientMsg.commit(session: "S", message: "Fix it", push: true)), encoding: .utf8)!
        let back = try JSONSerialization.jsonObject(with: Data(sent.utf8)) as! [String: Any]
        #expect(back["type"] as? String == "commit")
        #expect(back["message"] as? String == "Fix it")
        #expect(back["push"] as? Bool == true)

        let done = try decode(ServerMsg.self, #"{"type":"committed","session":"S","commit":"abc1234 Fix it","pushed":true}"#)
        guard case .committed(_, let c, let pushed) = done else { Issue.record("\(done)"); return }
        #expect(c == "abc1234 Fix it" && pushed)
        #expect(CommitText.result(commit: c, pushed: pushed) == "Committed abc1234 Fix it and pushed")

        let ch = try decode(ServerMsg.self, #"{"type":"changes","session":"S","diff":"","ahead":2}"#)
        guard case .changes(_, _, _, _, let ahead) = ch else { Issue.record("\(ch)"); return }
        #expect(ahead == 2)
        let none = try decode(ServerMsg.self, #"{"type":"changes","session":"S","diff":""}"#)
        guard case .changes(_, _, _, _, nil) = none else { Issue.record("no upstream: \(none)"); return }
    }

    @Test func agentMessagesAreRecognised() {
        let text = "Message from Claude (styrman), sent through Styrman:\n\nwhat does the parser do with tabs?\n\n(Reply as you normally would; your reply goes back to Claude.)"
        let m = AgentMessage(text)
        #expect(m?.sender == "Claude (styrman)")
        #expect(m?.agent == .claude)
        #expect(m?.body == "what does the parser do with tabs?")
        #expect(AgentMessage("just a normal prompt") == nil)
    }

    @Test func lanRouteNeedsAPrivateAddressAndAFingerprint() {
        let fp = String(repeating: "0f", count: 32)
        #expect(LanRoute(urls: ["wss://10.1.2.3:4720/v1/ws"], cert: fp) != nil)
        #expect(LanRoute(urls: ["wss://172.20.0.9:4720/v1/ws"], cert: fp) != nil)
        #expect(LanRoute(urls: ["wss://172.32.0.9:4720/v1/ws"], cert: fp) == nil)
        #expect(LanRoute(urls: ["wss://10.1.2.3:4720/v1/ws"], cert: nil) == nil)
        #expect(LanRoute(urls: ["wss://10.1.2.3:4720/v1/ws"], cert: fp.uppercased()) == nil)
        #expect(LanRoute(urls: ["wss://10.1.2.3:4720/v1/ws"], cert: "abc") == nil)
        #expect(LanPinner.fingerprint(Data("abc".utf8)) == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    }

    @Test func previewCarriesTheStatusLine() throws {
        let json = #"{"type":"preview","session":"s1","status":{"label":"Crunching…","elapsed_secs":1085,"tokens":"↓ 73.6k tokens","thinking":true}}"#
        guard case .preview(let sid, let text, let status?) = try JSONDecoder().decode(ServerMsg.self, from: Data(json.utf8)) else {
            Issue.record("no status"); return
        }
        #expect(sid == "s1" && text == nil)
        #expect(status.label == "Crunching…")
        #expect(status.detail() == "18m 5s · ↓ 73.6k tokens · thinking")
        #expect(status.detail(extra: 60) == "19m 5s · ↓ 73.6k tokens · thinking")
        #expect(WorkStatus(label: "Working", elapsedSecs: 3725, note: "running hooks").detail() == "running hooks · 1h 2m 5s")
        // An older daemon: no status.
        guard case .preview(_, _, nil) = try JSONDecoder().decode(ServerMsg.self, from: Data(#"{"type":"preview","session":"s1","text":"Hi"}"#.utf8)) else {
            Issue.record("status from nowhere"); return
        }
    }
}

@MainActor
struct StepsTests {
    func step(_ kind: ToolKind, _ status: ToolStatus = .completed) -> ToolItem {
        ToolItem(call: "c", tool: ToolInfo(name: "T", kind: kind, title: "t", input: .null, locations: nil),
                 status: status, output: nil)
    }

    @Test func aRunReadsLikeTheClaudeApp() {
        #expect(StepsRow.summary([step(.execute), step(.execute), step(.read), step(.execute, .failed)])
                == "Ran 3 commands, read a file (1 failed)")
        #expect(StepsRow.summary([step(.read), step(.read), step(.edit)]) == "Read 2 files, edited a file")
        #expect(StepsRow.summary([step(.search), step(.search)]) == "Searched 2 times")
    }

    @Test func thinkingShowsTheTimeInsteadOfTheWord() {
        let s = WorkStatus(label: "Brewing…", elapsedSecs: 20)
        #expect(WorkingRow.timeLine(s, extra: 0, thinkingFor: nil) == nil)
        var t = s; t.thinking = true
        #expect(WorkingRow.timeLine(t, extra: 2, thinkingFor: 1) == "22s · thinking…")
        #expect(WorkingRow.timeLine(t, extra: 6, thinkingFor: 9) == "26s · still thinking…")
        var d = s; d.note = "thought for 1s"
        #expect(WorkingRow.timeLine(d, extra: 2, thinkingFor: nil) == "22s · thought for 1s")
        #expect(WorkingRow.timeLine(nil, extra: 0, thinkingFor: nil) == nil)
    }
}

@MainActor
struct ShellHighlightTests {
    @Test func commandsAndStringsAreColoured() {
        let a = ShellHighlight.attributed("S=/tmp; tail -n 3 'a b' && rm x | head")
        func colour(of word: String) -> Color? {
            guard let r = a.range(of: word) else { return nil }
            return a[r].runs.first?.foregroundColor
        }
        #expect(String(a.characters) == "S=/tmp; tail -n 3 'a b' && rm x | head")
        #expect(colour(of: "tail") == ShellHighlight.command)
        #expect(colour(of: "rm") == ShellHighlight.command)
        #expect(colour(of: "head") == ShellHighlight.command)
        #expect(colour(of: "'a b'") == ShellHighlight.string)
        #expect(colour(of: "-n") == nil)
        #expect(colour(of: "S=/tmp") == nil)
    }
}

@MainActor
struct PeerNoteTests {
    @Test func notesBetweenAgentsAreRecognised() {
        let r = PeerNote("agy (styrman) replied: - **§710** — NOT YET")
        #expect(r == PeerNote(kind: .replied, agent: .antigravity, repo: "styrman", text: "- **§710** — NOT YET"))
        #expect(PeerNote("Asked Codex (reCORD): Review §705 (the plan)")?.text == "Review §705 (the plan)")
        #expect(PeerNote("Asked Codex (reCORD): Review §705")?.kind == .asked(started: false))
        #expect(PeerNote("Started Codex (my app) for this and asked: Check it")?.repo == "my app")
        #expect(PeerNote("No reply from Claude (reCORD) in time")?.kind == .noReply)
        #expect(PeerNote("A subagent reported back") == nil)
    }

    @Test func anAskIsOpenUntilItsReply() {
        let items: [TimelineItem] = [
            .notice(id: "1", text: "Asked Codex (reCORD): one", warning: false),
            .notice(id: "2", text: "Asked agy (styrman): two", warning: false),
            .notice(id: "3", text: "Codex (reCORD) replied: done", warning: false),
        ]
        #expect(PeerNote.openAsks(items) == ["2"])
    }
}

@MainActor
struct ExchangeTests {
    @Test func eachReplyPairsWithItsAsk() {
        let items: [TimelineItem] = [
            .notice(id: "1", text: "Asked Codex (reCORD): first?", warning: false),
            .notice(id: "2", text: "Asked agy (styrman): second?", warning: false),
            .notice(id: "3", text: "Codex (reCORD) replied: one", warning: false),
            .notice(id: "4", text: "Asked Codex (reCORD): again?", warning: false),
            .notice(id: "5", text: "No reply from agy (styrman) in time", warning: false),
        ]
        let x = PeerNote.exchanges(items)
        #expect(x == [PeerExchange(id: "3", askId: "1", agent: .codex, repo: "reCORD", question: "first?", answer: "one")])
        #expect(PeerNote.openAsks(items) == ["4"])
    }

    /// An old reply, cut at 400 characters, is found whole in the helper's conversation.
    @Test func aReplyCutShortIsFoundWhole() {
        let whole = "The parser drops the last row when the file has no trailing newline. Fix in read_rows."
        let cut = String(whole.prefix(40)) + "…"
        let helper: [TimelineItem] = [
            .assistant(id: "a", text: "Something else entirely, said earlier on.", at: .now),
            .assistant(id: "b", text: whole, at: .now),
        ]
        #expect(PeerExchange.full(cut, in: helper) == whole)
        #expect(PeerExchange.full(whole, in: helper) == nil, "not cut short")
        #expect(PeerExchange.full("Nothing like it in there, not at all, no…", in: helper) == nil)
    }
}

@MainActor
struct SessionRowTests {
    @Test func shortTimes() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        #expect(SessionCard.ago(now, now: now) == "now")
        #expect(SessionCard.ago(now.addingTimeInterval(-240), now: now) == "4m")
        #expect(SessionCard.ago(now.addingTimeInterval(-22 * 3600), now: now) == "22h")
        #expect(SessionCard.ago(now.addingTimeInterval(-3 * 86_400), now: now) == "3d")
    }

    @Test func aHelpersTitleIsItsQuestion() {
        #expect(SessionCard.topic("Message from Claude (reCORD), sent through Styrman:\n\nIs §704 fair?\n\n(Reply as you normally would; your reply goes back to Claude.)") == "Is §704 fair?")
        #expect(SessionCard.topic("Fix the\nparser") == "Fix the parser")
        #expect(SessionCard.topic("  ") == nil)
    }

    @Test func halfABoldPairIsDropped() {
        #expect(SessionCard.balanced("NOT YET.** Two issues") == "NOT YET. Two issues")
        #expect(SessionCard.balanced("a **bold** word") == "a **bold** word")
    }
}

@MainActor
struct SessionLineTests {
    private func s(_ id: String, helping: String? = nil, updated: Int64 = 0, status: String = "waiting") -> SessionSummary {
        let h = helping.map { #","helper_of":"\#($0)""# } ?? ""
        let json = #"{"id":"\#(id)","agent":"codex","native_id":"\#(id)","cwd":"/r","shared":true,"status":"\#(status)","created_ms":0,"updated_ms":\#(updated),"last_seq":0\#(h)}"#
        return try! JSONDecoder().decode(SessionSummary.self, from: Data(json.utf8))
    }

    /// Past two, a session's older helpers fold into "N more".
    @Test func olderHelpersFold() {
        let items = [s("P"), s("h1", helping: "P", updated: 1), s("h2", helping: "P", updated: 3),
                     s("h3", helping: "P", updated: 2), s("h4", helping: "P", updated: 0), s("Q")]
        #expect(SessionLine.lines(items, open: []).map(\.id) == ["P", "h2", "h3", "more-P", "Q"])
        #expect(SessionLine.lines(items, open: ["P"]).map(\.id) == ["P", "h2", "h3", "h1", "h4", "less-P", "Q"])
        // Three is no fold: "1 more" would take the same room.
        #expect(SessionLine.lines(Array(items.prefix(4)), open: []).count == 4)
        // A helper whose session isn't here stands alone.
        let lone = SessionLine.lines([s("h1", helping: "X")], open: [])
        #expect(lone == [.session(s("h1", helping: "X"), parent: nil)])
    }

    @Test func titlesArePlain() {
        #expect(SessionCard.plain("**NOT YET.** Two `issues`") == "NOT YET. Two issues")
    }

    /// A helper that answered goes with the agent that asked it, not "Needs attention";
    /// one asking for an approval still needs you.
    @Test func aRestingHelperGoesWithItsAgent() {
        let parent = s("P", status: "working")
        let all = [parent, s("h", helping: "P"), s("a", helping: "P", status: "needs_approval"), s("o", helping: "gone")]
        #expect(SessionSection.of(all[1], in: all) == .working)
        #expect(SessionSection.of(all[2], in: all) == .needsYou)
        #expect(SessionSection.of(all[3], in: all) == .done)
        #expect(SessionSection.of(s("me"), in: all) == .needsYou, "your own waiting session")
    }

    /// A helper's helper, and two sessions that asked each other, still show.
    @Test func noSessionVanishes() {
        let chain = [s("A"), s("B", helping: "A"), s("C", helping: "B")]
        #expect(Set(SessionLine.lines(chain, open: []).map(\.id)) == ["A", "B", "C"])
        let cycle = [s("A", helping: "B"), s("B", helping: "A"), s("Q")]
        let ids = SessionLine.lines(cycle, open: []).map(\.id)
        #expect(Set(ids) == ["A", "B", "Q"])
        #expect(ids.count == 3, "each once")
    }
}

struct FixesTests {
    /// The working star never turns into an emoji (a blinking green square).
    @Test func theSpinnerHasNoEmoji() {
        for f in WorkingDots.frames {
            #expect(!f.unicodeScalars.contains { $0.properties.isEmoji }, "\(f)")
        }
    }

    /// An option kind from a newer Mac doesn't hide the whole approval.
    @Test func anUnknownOptionKindStillDecodes() throws {
        let o = try JSONDecoder().decode(PermissionOption.self,
                                         from: Data(#"{"id":"x","kind":"allow_session","label":"Yes, this session"}"#.utf8))
        #expect(o.kind == .other)
        #expect(!o.kind.isAllow)
    }

    /// Attachments never share a name: they're saved side by side on the Mac.
    @Test func attachmentNamesAreUnique() {
        var list: [Styrman.Attachment] = []
        for _ in 0..<3 { list.append(Styrman.Attachment(name: Composer.unique("photo.jpg", in: list), data: Data(), isImage: true)) }
        #expect(list.map(\.name) == ["photo.jpg", "photo-2.jpg", "photo-3.jpg"])
        list.remove(at: 0)
        #expect(Composer.unique("photo.jpg", in: list) == "photo.jpg")
        let a = [Styrman.Attachment(name: "Shot.png", data: Data(), isImage: true)]
        #expect(Composer.unique("Shot.png", in: a) == "Shot-2.png")
        #expect(Composer.unique("notes", in: [Styrman.Attachment(name: "notes", data: Data(), isImage: false)]) == "notes-2")
    }
}

struct UsagePaceTests {
    private func window(_ json: String) throws -> UsageWindow {
        try JSONDecoder().decode(UsageWindow.self, from: Data(json.utf8))
    }

    /// Karv's pace lines: reserve, over with a run-out time, on pace, rolling.
    @Test func paceReadsAsKarvWritesIt() throws {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let under = try window(#"{"title":"5h","used_fraction":0.2,"resets_at_ms":1800003600000,"rolling":false,"pace":{"state":"under","reserve":0.11,"reserve_seconds":1920}}"#)
        #expect(UsageText.pace(under, now: now).text == "11% reserve · 32m")
        let over = try window(#"{"title":"Weekly","used_fraction":0.5,"resets_at_ms":1800400000000,"rolling":false,"pace":{"state":"over","reserve":-0.09,"reserve_seconds":-40000,"exhausts_at_ms":1800220000000}}"#)
        #expect(UsageText.pace(over, now: now).text == "9% over · out in 2d 13h")
        let rolling = try window(#"{"title":"5h","used_fraction":0.05,"resets_at_ms":1800001380000,"rolling":true}"#)
        #expect(UsageText.pace(rolling, now: now).text == "Rolling · frees up in 23m")
        // An older Karv: no pace, nothing said.
        let old = try window(#"{"title":"5h","used_fraction":0.2,"resets_at_ms":1800003600000,"rolling":false}"#)
        #expect(UsageText.pace(old, now: now).text.isEmpty)
        #expect(UsageText.percent(0.925) == "93%")
    }
}

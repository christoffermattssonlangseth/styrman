// JSON samples here are the exact shapes pinned by crates/styr-core/tests/wire_shape.rs
// and crates/styr-core/src/phone.rs: if one side changes, both test suites must.

import Foundation
import Testing
@testable import Styrman

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
        guard case .hello(let p, _, let d, let h) = hello else { Issue.record("not hello"); return }
        #expect((p, d, h) == (1, "D", "mac"))
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

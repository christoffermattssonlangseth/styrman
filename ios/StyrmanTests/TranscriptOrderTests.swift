import Foundation
import Testing
@testable import Styrman

private func event(_ seq: UInt64, ms: Int64, source: String = "hook", _ kind: String) throws -> Event {
    let json = #"{"session":"S","seq":\#(seq),"at_ms":\#(ms),"source":"\#(source)","kind":\#(kind)}"#
    return try JSONDecoder().decode(Event.self, from: Data(json.utf8))
}

private let tool = #"{"type":"tool_call","call":"c1","tool":{"name":"Bash","kind":"execute","title":"Run `sleep 4`","input":{}},"status":"running"}"#

@Suite @MainActor struct TranscriptOrderTests {
    /// Claude announces a tool (hook) just before writing the text that came first;
    /// that text, read from the transcript, goes above the tool.
    @Test func transcriptTextGoesAboveAToolThatJustStarted() throws {
        let t = SessionTimeline(sessionId: "S")
        t.apply([
            try event(1, ms: 1_000, #"{"type":"user_message","text":"go","origin":{"from":"terminal"}}"#),
            try event(2, ms: 2_000, tool),
            try event(3, ms: 2_800, source: "transcript",
                      #"{"type":"assistant_message","message":"m1","text":"Starting now.","mode":"final"}"#),
        ])
        let kinds = t.items.map { item -> String in
            switch item {
            case .user: "user"
            case .assistant: "assistant"
            case .tool: "tool"
            default: "other"
            }
        }
        #expect(kinds == ["user", "assistant", "tool"])
        // The tool still updates in place (its index moved).
        t.apply([try event(4, ms: 6_000, #"{"type":"tool_call","call":"c1","tool":{"name":"Bash","kind":"execute","title":"Run `sleep 4`","input":{}},"status":"completed"}"#)])
        guard case .tool(_, let done) = t.items[2] else { Issue.record("tool moved"); return }
        #expect(done.status == .completed)
        #expect(t.items.count == 3)
    }

    /// Text written well after a tool started (it ran a while) stays below it.
    @Test func laterTextStaysBelowAnOlderTool() throws {
        let t = SessionTimeline(sessionId: "S")
        t.apply([
            try event(1, ms: 2_000, tool),
            try event(2, ms: 9_000, source: "transcript",
                      #"{"type":"assistant_message","message":"m1","text":"Found it.","mode":"final"}"#),
        ])
        guard case .assistant = t.items.last else { Issue.record("text should stay last"); return }
    }

    /// The reply being written shows until the real message arrives, and a finished
    /// reply redrawn on the Mac's screen doesn't come back as a preview.
    @Test func previewUntilTheMessageArrives() throws {
        let t = SessionTimeline(sessionId: "S")
        t.showPreview("Lighthouses trace their origins")
        #expect(t.preview == "Lighthouses trace their origins")
        t.showPreview("Lighthouses trace their origins to antiquity, when")
        #expect(t.preview?.hasSuffix("when") == true)
        t.apply([try event(1, ms: 5_000, source: "transcript",
                           #"{"type":"assistant_message","message":"m1","text":"Lighthouses trace their origins to antiquity, when builders raised towers.","mode":"final"}"#)])
        #expect(t.preview == nil)
        t.showPreview("Lighthouses trace their origins to antiquity, when builders raised towers.")
        #expect(t.preview == nil, "no echo of what's already there")
        t.showPreview(nil)
        #expect(t.preview == nil)
    }

    /// A terminal flipping between a longer and a shorter reading (a blinking
    /// marker) mustn't make the preview, and the conversation, jump.
    @MainActor @Test func aFlipFloppingPreviewHoldsStill() async throws {
        let t = SessionTimeline(sessionId: "S")
        let long = "While they review, I'll start.\n\nWriting the helpers"
        t.showPreview(long)
        t.showPreview("Writing the helpers")
        #expect(t.preview == long, "a shorter reading waits")
        t.showPreview(long)
        try await Task.sleep(for: .milliseconds(1_200))
        #expect(t.preview == long, "it came back: the shorter one never showed")
        t.showPreview(nil)
        #expect(t.preview == long)
        try await Task.sleep(for: .milliseconds(1_200))
        #expect(t.preview == nil, "gone for real after a moment")
    }
}

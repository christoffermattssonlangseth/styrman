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

    /// A long message from the phone comes back from Claude rewrapped as a paste:
    /// the "Handed to agent" bubble still goes away.
    @Test func aSentMessageClearsEvenIfItComesBackChanged() throws {
        let t = SessionTimeline(sessionId: "S")
        t.addPending("Not nothing.\n\nFour reasons.")
        t.addPending("next")
        t.markDelivered("Not nothing.\n\nFour reasons.")
        t.apply([try event(1, ms: 1_000, #"{"type":"user_message","text":"\n\n<pasted_content id=\"7\">\nNot nothing.\n\nFour reasons.\n</pasted_content id=\"7\">","origin":{"from":"terminal"}}"#)])
        #expect(t.pending.map(\.text) == ["next"])
        guard case .user(_, let shown, _, _) = t.items.last else { Issue.record("no message"); return }
        #expect(shown == "Not nothing.\n\nFour reasons.")
        // Spacing changed: still the same message.
        t.apply([try event(2, ms: 2_000, #"{"type":"user_message","text":" next\n","origin":{"from":"terminal"}}"#)])
        #expect(t.pending.isEmpty)
    }

    /// A screen still showing the reply before a step (and an older `styr` reading
    /// Claude's status line as text) previews only what's new.
    @Test func aPreviewShowsOnlyWhatsNew() throws {
        let t = SessionTimeline(sessionId: "S")
        t.apply([try event(1, ms: 1_000, source: "transcript",
                           #"{"type":"assistant_message","message":"m1","text":"A JSON key bug (tuple keys from `groupby`). Fixing it:","mode":"final"}"#)])
        t.showPreview("A JSON key bug (tuple keys from groupby). Fixing it:\n\nFixing JSON keys and rerunning smoke · 1m 47s")
        #expect(t.preview == nil)
        t.showPreview("A JSON key bug (tuple keys from groupby). Fixing it:\n\nAll green now.")
        #expect(t.preview == "All green now.")
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
        for _ in 0..<30 where t.preview != nil { try await Task.sleep(for: .milliseconds(100)) }
        #expect(t.preview == nil, "gone for real after a moment")
    }

    /// Live, a step that starts while its text still shows as the preview doesn't
    /// blank it: the text turns into the message in place, the step shows after.
    @Test func theTextBeforeAStepStaysUntilItArrives() async throws {
        let t = SessionTimeline(sessionId: "S")
        t.isLive = true
        t.showPreview("I'll read the notes first.")
        t.apply([try event(1, ms: 2_000, tool)])
        #expect(t.preview == "I'll read the notes first.")
        #expect(!t.shownItems.contains { if case .tool = $0 { true } else { false } }, "the step waits")
        t.apply([try event(2, ms: 1_900, source: "transcript",
                           #"{"type":"assistant_message","message":"m1","text":"I'll read the notes first.","mode":"final"}"#)])
        #expect(t.preview == nil)
        for _ in 0..<30 where t.shownItems.count < 2 { try await Task.sleep(for: .milliseconds(100)) }
        let kinds = t.shownItems.map { item -> String in
            switch item { case .assistant: "assistant"; case .tool: "tool"; default: "other" }
        }
        #expect(kinds == ["assistant", "tool"])
    }

    /// If no text comes, the preview left from before the step goes when it shows.
    @Test func aLeftoverPreviewGoesWhenTheStepShows() async throws {
        let t = SessionTimeline(sessionId: "S")
        t.isLive = true
        t.showPreview("Checking.")
        t.apply([try event(1, ms: 2_000, tool)])
        // The step is held back 1.3 s; on a busy test machine the release can run
        // late, so wait for it rather than a fixed time.
        for _ in 0..<30 where t.preview != nil { try await Task.sleep(for: .milliseconds(100)) }
        #expect(t.preview == nil)
    }

    /// A command left running in the background counts until Claude says it's done;
    /// a compaction doesn't end it, a new Claude process does.
    @Test func backgroundTasksRunUntilTheyEnd() throws {
        let now = Int64(SessionTimeline.tasksRecordedSince.timeIntervalSince1970 * 1000) + 60_000
        let bg = #"{"type":"tool_call","call":"b1","tool":{"name":"Bash","kind":"execute","title":"Run `make`","input":{"command":"make","description":"Build it","run_in_background":true}},"status":"completed"}"#
        let agent = #"{"type":"tool_call","call":"a1","tool":{"name":"Agent","kind":"other","title":"Agent","input":{"description":"Review the parser","run_in_background":true}},"status":"completed"}"#
        let t = SessionTimeline(sessionId: "S")
        t.apply([try event(1, ms: now, bg), try event(2, ms: now + 1, agent), try event(3, ms: now + 2, tool)])
        #expect(t.runningTasks.map(\.title) == ["Build it", "Review the parser"])
        #expect(t.runningTasks.map(\.isAgent) == [false, true])
        t.apply([try event(4, ms: now + 3, #"{"type":"session_started","start":"compact"}"#),
                 try event(5, ms: now + 4, #"{"type":"task_ended","call":"b1","status":"completed","summary":"Background command \"Build it\" completed (exit code 0)"}"#)])
        #expect(t.runningTasks.map(\.id) == ["a1"])
        #expect(t.tasks.first?.ended?.summary == "Background command \"Build it\" completed (exit code 0)")
        t.apply([try event(6, ms: now + 5, #"{"type":"session_ended"}"#)])
        #expect(t.runningTasks.isEmpty)
        // From before endings were recorded: not listed.
        let old = SessionTimeline(sessionId: "S")
        old.apply([try event(1, ms: 2_000, bg)])
        #expect(old.tasks.isEmpty)
    }
}


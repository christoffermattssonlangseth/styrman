package com.christoffermattssonlangseth.styrman

import com.christoffermattssonlangseth.styrman.ui.DisplayRow
import com.christoffermattssonlangseth.styrman.ui.PeerExchange
import com.christoffermattssonlangseth.styrman.ui.PeerNote
import com.christoffermattssonlangseth.styrman.ui.ShellHighlight
import com.christoffermattssonlangseth.styrman.ui.Steps
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The conversation view's steps, tasks and agents talking, as on iOS. */
class ConversationTest {
    private fun step(kind: ToolKind, status: ToolStatus = ToolStatus.Completed, id: String = "c") =
        TimelineItem.Tool(id, id, ToolInfo("T", kind, "t", JsonNull), status, null)

    @Test fun aRunReadsLikeTheClaudeApp() {
        assertEquals("Ran 3 commands, read a file (1 failed)",
            Steps.summary(listOf(step(ToolKind.Execute), step(ToolKind.Execute), step(ToolKind.Read), step(ToolKind.Execute, ToolStatus.Failed))))
        assertEquals("Read 2 files, edited a file", Steps.summary(listOf(step(ToolKind.Read), step(ToolKind.Read), step(ToolKind.Edit))))
        assertEquals("Searched 2 times", Steps.summary(listOf(step(ToolKind.Search), step(ToolKind.Search))))
    }

    @Test fun everyRunOfStepsIsOneRow() {
        val items = listOf(
            step(ToolKind.Execute, id = "1"), step(ToolKind.Read, id = "2"),
            TimelineItem.Assistant("3", "Done"),
            step(ToolKind.Edit, id = "4"),
        )
        val rows = DisplayRow.rows(items)
        assertEquals(listOf("1", "3", "4"), rows.map { it.id })
        assertEquals(2, (rows[0] as DisplayRow.Tools).tools.size)
        assertEquals(1, (rows[2] as DisplayRow.Tools).tools.size)
    }

    @Test fun longOutputIsCut() {
        val t = step(ToolKind.Execute).copy(output = "x".repeat(Steps.MAX_OUTPUT + 10))
        assertEquals(Steps.MAX_OUTPUT + 2, Steps.outputText(t).length)
        assertEquals("Still running…", Steps.outputText(step(ToolKind.Execute, ToolStatus.Running)))
        assertEquals("(no output)", Steps.outputText(step(ToolKind.Execute)))
    }

    @Test fun commandsAndStringsAreColoured() {
        val cmd = "S=/tmp; tail -n 3 'a b' && rm x | head"
        val segs = ShellHighlight.segments(cmd)
        assertEquals(cmd, segs.joinToString("") { it.first })
        fun kind(word: String) = segs.firstOrNull { it.first == word }?.second
        assertEquals(ShellHighlight.Kind.Command, kind("tail"))
        assertEquals(ShellHighlight.Kind.Command, kind("rm"))
        assertEquals(ShellHighlight.Kind.Command, kind("head"))
        assertEquals(ShellHighlight.Kind.Str, kind("'a b'"))
        assertEquals(ShellHighlight.Kind.Plain, kind("-n"))
        assertEquals(ShellHighlight.Kind.Plain, kind("S=/tmp"))
    }

    @Test fun notesBetweenAgentsAreRecognised() {
        assertEquals(PeerNote(PeerNote.Kind.Replied, AgentKind.Antigravity, "styrman", "- **§710** — NOT YET"),
            PeerNote.parse("agy (styrman) replied: - **§710** — NOT YET"))
        assertEquals("Review §705 (the plan)", PeerNote.parse("Asked Codex (reCORD): Review §705 (the plan)")?.text)
        assertEquals(PeerNote.Kind.Asked, PeerNote.parse("Asked Codex (reCORD): Review §705")?.kind)
        assertEquals("my app", PeerNote.parse("Started Codex (my app) for this and asked: Check it")?.repo)
        assertEquals(PeerNote.Kind.Started, PeerNote.parse("Started Codex (my app) for this and asked: Check it")?.kind)
        assertEquals(PeerNote.Kind.NoReply, PeerNote.parse("No reply from Claude (reCORD) in time")?.kind)
        assertNull(PeerNote.parse("A subagent reported back"))
    }

    private fun notice(id: String, text: String) = TimelineItem.Notice(id, text, false)

    @Test fun anAskIsOpenUntilItsReply() {
        val items = listOf(notice("1", "Asked Codex (reCORD): one"), notice("2", "Asked agy (styrman): two"),
            notice("3", "Codex (reCORD) replied: done"))
        assertEquals(setOf("2"), PeerNote.openAsks(items))
    }

    @Test fun eachReplyPairsWithItsAsk() {
        val items = listOf(
            notice("1", "Asked Codex (reCORD): first?"),
            notice("2", "Asked agy (styrman): second?"),
            notice("3", "Codex (reCORD) replied: one"),
            notice("4", "Asked Codex (reCORD): again?"),
            notice("5", "No reply from agy (styrman) in time"),
        )
        assertEquals(listOf(PeerExchange("3", "1", AgentKind.Codex, "reCORD", "first?", "one")), PeerNote.exchanges(items))
        assertEquals(setOf("4"), PeerNote.openAsks(items))
    }

    @Test fun aReplyCutShortIsFoundWhole() {
        val whole = "The parser drops the last row when the file has no trailing newline. Fix in read_rows."
        val cut = whole.take(40) + "…"
        val helper = listOf(TimelineItem.Assistant("a", "Something else entirely, said earlier on."), TimelineItem.Assistant("b", whole))
        assertEquals(whole, PeerExchange.full(cut, helper))
        assertNull("not cut short", PeerExchange.full(whole, helper))
        assertNull(PeerExchange.full("Nothing like it in there, not at all, no…", helper))
    }

    private fun event(seq: Long, ms: Long, kind: String) =
        Event.parse(Json.parseToJsonElement("""{"session":"S","seq":$seq,"at_ms":$ms,"source":"t","kind":$kind}""") as JsonObject)

    @Test fun backgroundTasksRunUntilTheyEnd() {
        val now = SessionTimeline.TASKS_RECORDED_SINCE_MS + 60_000
        val bg = """{"type":"tool_call","call":"b1","tool":{"name":"Bash","kind":"execute","title":"Run `make`","input":{"command":"make","description":"Build it","run_in_background":true}},"status":"completed"}"""
        val agent = """{"type":"tool_call","call":"a1","tool":{"name":"Agent","kind":"other","title":"Agent","input":{"description":"Review the parser","run_in_background":true}},"status":"completed"}"""
        val plain = """{"type":"tool_call","call":"p1","tool":{"name":"Bash","kind":"execute","title":"ls","input":{"command":"ls"}},"status":"completed"}"""
        val t = SessionTimeline("S")
        t.apply(listOf(event(1, now, bg), event(2, now + 1, agent), event(3, now + 2, plain)))
        assertEquals(listOf("Build it", "Review the parser"), t.runningTasks.map { it.title })
        assertEquals(listOf(false, true), t.runningTasks.map { it.isAgent })
        t.apply(listOf(
            event(4, now + 3, """{"type":"session_started","start":"compact"}"""),
            event(5, now + 4, """{"type":"task_ended","call":"b1","status":"completed","summary":"Background command \"Build it\" completed (exit code 0)"}"""),
        ))
        assertEquals(listOf("a1"), t.runningTasks.map { it.id })
        assertEquals("Background command \"Build it\" completed (exit code 0)", t.tasks.first().ended?.summary)
        t.apply(listOf(event(6, now + 5, """{"type":"session_ended"}""")))
        assertTrue(t.runningTasks.isEmpty())
        // From before endings were recorded: not listed.
        val old = SessionTimeline("S")
        old.apply(listOf(event(1, 2_000, bg)))
        assertTrue(old.tasks.isEmpty())
    }

    @Test fun aSessionSaysWhoItHelps() {
        val s = SessionSummary.parse(Json.parseToJsonElement("""{"id":"H","agent":"codex","helper_of":"S"}""") as JsonObject)
        assertEquals("S", s.helperOf)
    }
}

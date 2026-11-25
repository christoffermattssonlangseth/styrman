package com.christoffermattssonlangseth.styrman

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** JSON samples are the shapes pinned by crates/styr-core/tests/wire_shape.rs (and the iOS tests). */
class ProtocolTest {
    @Test
    fun sessionsAndEventsDecode() {
        val msg = ServerMsg.parse(
            """{"type":"sessions","sessions":[{"id":"S","agent":"codex","native_id":"n","cwd":"/Users/me/work/app",
               "title":"fix it","model":null,"shared":true,"status":"needs_approval","created_ms":1,"updated_ms":2,
               "last_seq":3,"in_terminal_wrapper":true,"accepts_prompts":true}]}""",
        ) as ServerMsg.Sessions
        val s = msg.sessions.single()
        assertEquals(AgentKind.Codex, s.agent)
        assertEquals(SessionStatus.NeedsApproval, s.status)
        assertEquals("app", s.repoName)

        val ev = ServerMsg.parse(
            """{"type":"event","event":{"session":"S","seq":4,"at_ms":5,"source":"pty","kind":{"type":"permission_requested",
               "request":"r1","call":null,"tool":{"name":"command","kind":"execute","title":"Run `touch x`","input":{"command":"touch x"},"locations":[]},
               "reason":"Would you like to run the following command?","options":[{"id":"opt-0","kind":"allow_once","label":"Yes, proceed"}],
               "expires_at_ms":null}}}""",
        ) as ServerMsg.EventMsg
        val k = ev.event.kind as EventKind.PermissionRequested
        assertEquals("r1", k.request)
        assertEquals(ToolKind.Execute, k.tool.kind)
        assertEquals(OptionKind.AllowOnce, k.options.single().kind)
    }

    @Test
    fun unknownTypesDoNotBreakAnything() {
        assertTrue(ServerMsg.parse("""{"type":"something_new"}""") is ServerMsg.Unknown)
        val e = EventKind.parse(kotlinx.serialization.json.Json.parseToJsonElement("""{"type":"future_kind"}""") as kotlinx.serialization.json.JsonObject)
        assertTrue(e is EventKind.Unknown)
    }

    @Test
    fun timelineBuildsLikeIos() {
        val t = SessionTimeline("S")
        fun ev(seq: Long, kind: String) = Event.parse(
            kotlinx.serialization.json.Json.parseToJsonElement("""{"session":"S","seq":$seq,"at_ms":0,"source":"hook","kind":$kind}""")
                as kotlinx.serialization.json.JsonObject,
        )
        t.addPending("hi")
        t.apply(listOf(
            ev(1, """{"type":"user_message","text":"hi","origin":{"from":"remote","device":"d"}}"""),
            ev(2, """{"type":"tool_call","call":"c1","tool":{"name":"Bash","kind":"execute","title":"Run","input":{}},"status":"pending"}"""),
            ev(3, """{"type":"tool_call","call":"c1","tool":{"name":"Bash","kind":"execute","title":"Run","input":{}},"status":"completed","output":{"type":"text","text":"ok","truncated":false}}"""),
            ev(4, """{"type":"permission_requested","request":"r","tool":{"name":"x","kind":"other","title":"T","input":null},"options":[]}"""),
            ev(5, """{"type":"permission_resolved","request":"r","outcome":{"outcome":"selected","option":"a","kind":"allow_once"},"by":{"by":"terminal"}}"""),
        ))
        assertTrue("the phone's prompt was picked up", t.pending.isEmpty())
        val tool = t.items[1] as TimelineItem.Tool
        assertEquals(ToolStatus.Completed, tool.status)
        assertEquals("ok", tool.output)
        assertEquals(3, t.items.size)
        assertEquals("Allowed in the terminal", (t.items[2] as TimelineItem.Permission).resolution)
        // Replays are ignored.
        t.apply(listOf(ev(2, """{"type":"notice","text":"x","level":"info"}""")))
        assertEquals(3, t.items.size)
    }

    @Test
    fun pairingLinks() {
        val l = PairingLink.parse("styrman://pair?v=1&url=ws%3A%2F%2Fmac.tail.ts.net%3A4719%2Fv1%2Fws&code=ABCD1234&name=My%20Mac")!!
        assertEquals("ws://mac.tail.ts.net:4719/v1/ws", l.wsUrl)
        assertEquals("http://mac.tail.ts.net:4719/", l.httpBase)
        assertEquals("ABCD1234", l.code)
        assertEquals("My Mac", l.name)
        assertNull(PairingLink.parse("https://example.com"))
        assertNull(PairingLink.parse("styrman://pair?url=http%3A%2F%2Fx&code=A"))
        // Pasted with text around it.
        assertEquals("ABCD1234", PairingLink.parse("link: styrman://pair?v=1&url=ws%3A%2F%2Fh%3A1%2Fv1%2Fws&code=ABCD1234&name=M thanks")?.code)
    }

    @Test
    fun repliesReadNaturally() {
        assertEquals("I changed pty.rs to it builds", SpeechText.clean("I changed crates/styr/src/pty.rs -> it builds"))
        assertTrue(SpeechText.clean("Done:\n```rust\nfn main() {}\n```").contains("(code block)"))
        assertEquals("Open a link now", SpeechText.clean("Open https://github.com/a/b now"))
        assertEquals(listOf("First.", "second"), SpeechText.paragraphs("First.\n\n second "))
    }
}

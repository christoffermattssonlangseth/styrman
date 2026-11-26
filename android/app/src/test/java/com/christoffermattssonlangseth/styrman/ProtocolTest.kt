package com.christoffermattssonlangseth.styrman

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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

    @Test
    fun agentMessagesAreRecognised() {
        val m = com.christoffermattssonlangseth.styrman.ui.agentMessage(
            "Message from Codex (app), sent through Styrman:\n\nreview this\n\n(Reply as you normally would; your reply goes back to Codex.)",
        )!!
        assertEquals("Codex (app)", m.sender)
        assertEquals(AgentKind.Codex, m.agent)
        assertEquals("review this", m.body)
        assertNull(com.christoffermattssonlangseth.styrman.ui.agentMessage("hello"))
    }

    @Test
    fun sessionModeAndOutdated() {
        val base = """"id":"S","agent":"claude","native_id":"n","cwd":null,"title":null,"model":null,"shared":true,
            "status":"working","created_ms":1,"updated_ms":2,"last_seq":3,"in_terminal_wrapper":true,"accepts_prompts":true"""
        val s = (ServerMsg.parse("""{"type":"session_updated","session":{$base,"mode":"acceptEdits","outdated":true}}""") as ServerMsg.SessionUpdated).session
        assertEquals(PermissionMode.AcceptEdits, s.permissionMode)
        assertEquals("Edits", s.permissionMode!!.short)
        assertTrue(s.outdated)
        assertFalse("no stop or terminal in an outdated wrapper", s.hasLiveControls)
        assertTrue(s.canEnd)
        // Both fields are left out when unset.
        val plain = (ServerMsg.parse("""{"type":"session_updated","session":{$base}}""") as ServerMsg.SessionUpdated).session
        assertNull(plain.permissionMode)
        assertTrue(plain.hasLiveControls)
        assertFalse(plain.copy(status = SessionStatus.Done).canEnd)
        // Bypass is offered only when the session already runs in it.
        assertFalse(PermissionMode.BypassPermissions in PermissionMode.choices(PermissionMode.Manual))
        assertTrue(PermissionMode.BypassPermissions in PermissionMode.choices(PermissionMode.BypassPermissions))
    }

    @Test
    fun newServerMessagesDecode() {
        val c = ServerMsg.parse("""{"type":"changes","session":"S","branch":"main","diff":"diff --git a/x b/x\n","truncated":true}""") as ServerMsg.Changes
        assertEquals(ServerMsg.Changes("S", "main", "diff --git a/x b/x\n", true), c)
        val bare = ServerMsg.parse("""{"type":"changes","session":"S","diff":""}""") as ServerMsg.Changes
        assertNull(bare.branch)
        assertFalse(bare.truncated)
        assertEquals(ServerMsg.Screen("S", "$ ls", 120), ServerMsg.parse("""{"type":"screen","session":"S","text":"$ ls","cols":120}"""))
        assertEquals(ServerMsg.Preview("S", "Writ"), ServerMsg.parse("""{"type":"preview","session":"S","text":"Writ"}"""))
        assertEquals(ServerMsg.Preview("S", null), ServerMsg.parse("""{"type":"preview","session":"S"}"""))
    }

    @Test
    fun newCommandsEncode() {
        fun j(s: String) = Json.parseToJsonElement(s)
        assertEquals(j("""{"type":"command","session":"S","command":{"type":"interrupt"}}"""), ClientMsg.interrupt("S"))
        assertEquals(j("""{"type":"command","session":"S","command":{"type":"end_session"}}"""), ClientMsg.endSession("S"))
        assertEquals(j("""{"type":"command","session":"S","command":{"type":"set_mode","mode":"plan"}}"""), ClientMsg.setMode("S", "plan"))
        assertEquals(j("""{"type":"get_changes","session":"S"}"""), ClientMsg.getChanges("S"))
        assertEquals(j("""{"type":"watch_screen","session":"S"}"""), ClientMsg.watchScreen("S"))
        assertEquals(
            j("""{"type":"command","session":"S","command":{"type":"resolve_permission","request":"r","option":"o"}}"""),
            ClientMsg.resolvePermission("S", "r", "o"),
        )
    }

    @Test
    fun previewShowsUntilTheReplyArrives() {
        val t = SessionTimeline("S")
        fun ev(seq: Long, kind: String) = Event.parse(
            Json.parseToJsonElement("""{"session":"S","seq":$seq,"at_ms":0,"source":"hook","kind":$kind}""") as JsonObject,
        )
        t.isLive = true
        t.showPreview("Writing the fix")
        assertEquals("Writing the fix", t.preview)
        t.apply(listOf(ev(1, """{"type":"assistant_message","message":"m","text":"Writing the fix now.","mode":"full"}""")))
        assertNull("the real message replaces it", t.preview)
        // The terminal redrawing the finished reply isn't shown again.
        t.showPreview("…the   fix now.")
        assertNull(t.preview)
        t.showPreview("Next")
        t.isLive = false
        assertNull("offline, nothing keeps it current", t.preview)
    }

    @Test
    fun claudeEditsBecomeDiffs() {
        fun tool(name: String, input: String) =
            ToolInfo(name, ToolKind.Edit, name, Json.parseToJsonElement(input))
        val edit = FileDiff.from(tool("Edit", """{"file_path":"/r/a.rs","old_string":"a\nb\nc\n","new_string":"a\nB\nc\n"}"""))!!.single()
        assertEquals("/r/a.rs", edit.path)
        assertEquals(
            listOf(FileDiff.Kind.Context, FileDiff.Kind.Removed, FileDiff.Kind.Added, FileDiff.Kind.Context),
            edit.lines.map { it.kind },
        )
        assertEquals(1 to 1, edit.added to edit.removed)

        val multi = FileDiff.from(tool("MultiEdit", """{"file_path":"f","edits":[{"old_string":"x","new_string":"y"},{"old_string":"p","new_string":"q"}]}"""))!!.single()
        assertEquals(FileDiff.Kind.Gap, multi.lines[2].kind)
        assertEquals(2, multi.added)

        val write = FileDiff.from(tool("Write", """{"file_path":"n.txt","content":"one\ntwo\n"}"""))!!.single()
        assertTrue(write.isNew)
        assertEquals(listOf("one", "two"), write.lines.map { it.text })

        assertNull(FileDiff.from(tool("Read", """{"file_path":"f"}""")))
        // Long unchanged stretches fold to two lines around a change.
        val long = (1..20).joinToString("\n") { "l$it" }
        val folded = FileDiff.lines(long, long.replace("l10\n", "L10\n"))
        assertEquals(listOf("l8", "l9", "l10", "L10", "l11", "l12"), folded.map { it.text })
    }

    @Test
    fun codexPatchesBecomeDiffs() {
        val patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@ fn main\n ctx\n-old\n+new\n" +
            "*** Add File: b.txt\n+hello\n*** Delete File: c.txt\n*** End Patch"
        val tool = ToolInfo("apply_patch", ToolKind.Edit, "Edit", kotlinx.serialization.json.JsonPrimitive(patch))
        val d = FileDiff.from(tool)!!
        assertEquals(listOf("src/a.rs", "b.txt", "c.txt"), d.map { it.path })
        assertEquals(listOf(FileDiff.Kind.Context, FileDiff.Kind.Removed, FileDiff.Kind.Added), d[0].lines.map { it.kind })
        assertTrue(d[1].isNew)
        assertTrue(d[2].isDeleted)
    }

    @Test
    fun gitDiffsSplitPerFileWithLineNumbers() {
        val diff = """
            diff --git a/src/x.rs b/src/x.rs
            index 1..2 100644
            --- a/src/x.rs
            +++ b/src/x.rs
            @@ -10,3 +10,3 @@ fn x
             keep
            -gone
            +here
            @@ -40,1 +40,2 @@
             a
            +b
            diff --git a/new.txt b/new.txt
            new file mode 100644
            --- /dev/null
            +++ b/new.txt
            @@ -0,0 +1 @@
            +hi
        """.trimIndent()
        val files = GitChanges.files(diff)
        assertEquals(listOf("src/x.rs", "new.txt"), files.map { it.path })
        val x = files[0]
        assertEquals(FileDiff.Line(FileDiff.Kind.Context, "keep", old = 10, new = 10), x.lines[0])
        assertEquals(FileDiff.Line(FileDiff.Kind.Removed, "gone", old = 11), x.lines[1])
        assertEquals(FileDiff.Line(FileDiff.Kind.Added, "here", new = 11), x.lines[2])
        assertEquals(FileDiff.Kind.Gap, x.lines[3].kind)
        assertEquals(FileDiff.Line(FileDiff.Kind.Added, "b", new = 41), x.lines[5])
        assertTrue(files[1].isNew)
        assertEquals(1, files[1].added)
    }

    @Test
    fun reviewCommentsComposeOneMessage() {
        val m = ReviewMessage.compose(listOf(
            ReviewComment("src/x.rs", FileDiff.Line(FileDiff.Kind.Added, "  let y = 1;", new = 42), "Name this better"),
            ReviewComment("a.txt", FileDiff.Line(FileDiff.Kind.Removed, "", old = 3), "Keep it\nplease"),
        ))
        assertEquals(
            "Review comments on the uncommitted changes:\n" +
                "\n1. `src/x.rs` line 42: `let y = 1;`\n   Name this better\n" +
                "\n2. `a.txt` (removed line 3)\n   Keep it\n   please\n",
            m,
        )
    }
}

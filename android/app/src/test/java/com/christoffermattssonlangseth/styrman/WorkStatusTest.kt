package com.christoffermattssonlangseth.styrman

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import com.christoffermattssonlangseth.styrman.ui.timeLine

class WorkStatusTest {
    @Test fun previewCarriesTheStatusLine() {
        val msg = ServerMsg.parse("""{"type":"preview","session":"s1","status":{"label":"Crunching…","elapsed_secs":1085,"tokens":"↓ 73.6k tokens","thinking":true}}""")
        val st = (msg as ServerMsg.Preview).status!!
        assertEquals("Crunching…", st.label)
        assertEquals("18m 5s · ↓ 73.6k tokens · thinking", st.detail())
        assertEquals("19m 5s · ↓ 73.6k tokens · thinking", st.detail(extra = 60))
        assertEquals("running hooks · 1h 2m 5s", WorkStatus("Working", 3725, note = "running hooks").detail())
        // An older daemon: no status.
        assertNull((ServerMsg.parse("""{"type":"preview","session":"s1","text":"Hi"}""") as ServerMsg.Preview).status)
    }

    @Test fun thinkingShowsTheTimeInsteadOfTheWord() {
        val s = WorkStatus("Brewing…", 20)
        assertNull(timeLine(s, 0, null))
        assertEquals("22s · thinking…", timeLine(s.copy(thinking = true), 2, 1))
        assertEquals("26s · still thinking…", timeLine(s.copy(thinking = true), 6, 9))
        assertEquals("22s · thought for 1s", timeLine(s.copy(note = "thought for 1s"), 2, null))
    }
}

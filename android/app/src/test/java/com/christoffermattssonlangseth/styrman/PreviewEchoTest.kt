package com.christoffermattssonlangseth.styrman

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PreviewEchoTest {
    @Test fun aReplyAlreadyHereIsNotShownAgainAsAPreview() {
        val said = "## Summary\n\nThe project is a **small harbour project** split across `note1.md` and `note2.md`."
        val items = listOf<TimelineItem>(TimelineItem.Assistant("1", said))
        // How the terminal reads it back: the heading bold, the code not marked.
        val screen = "**Summary**\n\nThe project is a **small harbour project** split across note1.md and note2.md."
        assertTrue(SessionTimeline.alreadySaid(screen, items))
        assertTrue(SessionTimeline.alreadySaid("…split across note1.md and note2.md.", items))
        assertFalse(SessionTimeline.alreadySaid("Something new that Claude is writing now.", items))
    }
}

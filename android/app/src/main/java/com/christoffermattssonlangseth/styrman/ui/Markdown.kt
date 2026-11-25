package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withStyle

/**
 * Inline Markdown, like the iOS app shows it: **bold**, *italic*, `code`, [links](…)
 * (link text only) and `#` headings in bold. Block structure (lists, code fences) stays
 * as plain lines.
 */
fun markdown(src: String, codeBackground: Color): AnnotatedString = buildAnnotatedString {
    val token = Regex("""\*\*(.+?)\*\*|__(.+?)__|`([^`]+)`|\[([^\]]+)]\(([^)]+)\)|(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?!\w)""")
    for ((n, rawLine) in src.split("\n").withIndex()) {
        if (n > 0) append("\n")
        val heading = Regex("^\\s{0,3}#{1,6}\\s+").find(rawLine)
        val line = heading?.let { rawLine.substring(it.range.last + 1) } ?: rawLine
        val start = length
        var i = 0
        for (m in token.findAll(line)) {
            append(line.substring(i, m.range.first))
            val g = m.groupValues
            when {
                g[1].isNotEmpty() -> withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(g[1]) }
                g[2].isNotEmpty() -> withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(g[2]) }
                g[3].isNotEmpty() -> withStyle(SpanStyle(fontFamily = Mono, background = codeBackground)) { append(g[3]) }
                g[4].isNotEmpty() -> withStyle(SpanStyle(textDecoration = TextDecoration.Underline)) { append(g[4]) }
                g[6].isNotEmpty() -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append(g[6]) }
            }
            i = m.range.last + 1
        }
        append(line.substring(i))
        if (heading != null) addStyle(SpanStyle(fontWeight = FontWeight.Bold), start, length)
    }
}

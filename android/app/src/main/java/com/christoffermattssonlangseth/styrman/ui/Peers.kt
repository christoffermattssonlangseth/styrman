package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.SwapHoriz
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.SessionStatus
import com.christoffermattssonlangseth.styrman.SessionSummary
import com.christoffermattssonlangseth.styrman.TimelineItem
import kotlinx.coroutines.delay

/** What the Mac notes in a session when its agent talks to another one:
 *  "Asked Codex (reCORD): …", "Started Codex (reCORD) for this and asked: …",
 *  "Codex (reCORD) replied: …", "No reply from Codex (reCORD) in time". */
data class PeerNote(val kind: Kind, val agent: AgentKind, val repo: String, val text: String) {
    enum class Kind { Asked, Started, Replied, NoReply }

    val isAsk get() = kind == Kind.Asked || kind == Kind.Started

    /** "Codex (reCORD)". */
    val who get() = "${agent.displayName} ($repo)"

    companion object {
        private fun split(s: String): Pair<AgentKind, String>? {
            val open = s.lastIndexOf('(')
            if (open < 0 || !s.endsWith(")")) return null
            val name = s.substring(0, open).trim().lowercase()
            val agent = when {
                name.startsWith("claude") -> AgentKind.Claude
                name.startsWith("codex") -> AgentKind.Codex
                name.startsWith("agy") || name.startsWith("antigravity") -> AgentKind.Antigravity
                else -> return null
            }
            return agent to s.substring(open + 1, s.length - 1)
        }

        fun parse(n: String): PeerNote? {
            val replied = n.indexOf(") replied: ")
            if (replied >= 0) split(n.substring(0, replied + 1))?.let { (a, r) ->
                return PeerNote(Kind.Replied, a, r, n.substring(replied + ") replied: ".length))
            }
            if (n.startsWith("Asked ")) {
                val i = n.indexOf("): ")
                if (i >= 0) split(n.substring(6, i + 1))?.let { (a, r) -> return PeerNote(Kind.Asked, a, r, n.substring(i + 3)) }
            }
            if (n.startsWith("Started ")) {
                val m = ") for this and asked: "
                val i = n.indexOf(m)
                if (i >= 0) split(n.substring(8, i + 1))?.let { (a, r) -> return PeerNote(Kind.Started, a, r, n.substring(i + m.length)) }
            }
            if (n.startsWith("No reply from ") && n.endsWith(" in time")) {
                split(n.substring(14, n.length - 8))?.let { (a, r) -> return PeerNote(Kind.NoReply, a, r, "") }
            }
            return null
        }

        /** Each reply with the ask it answers (the latest ask to that agent before it). */
        fun exchanges(items: List<TimelineItem>): List<PeerExchange> {
            val asks = HashMap<String, Pair<String, String>>()
            val out = mutableListOf<PeerExchange>()
            for (item in items) {
                val n = (item as? TimelineItem.Notice)?.let { parse(it.text) } ?: continue
                when {
                    n.isAsk -> asks[n.who] = item.id to n.text
                    n.kind == Kind.Replied -> asks.remove(n.who)?.let { (askId, q) ->
                        out += PeerExchange(item.id, askId, n.agent, n.repo, q, n.text)
                    }
                    else -> asks.remove(n.who)
                }
            }
            return out
        }

        /** The asks no reply (or "no reply in time") has come for yet, by notice id. */
        fun openAsks(items: List<TimelineItem>): Set<String> {
            val open = HashMap<String, String>()
            for (item in items) {
                val n = (item as? TimelineItem.Notice)?.let { parse(it.text) } ?: continue
                if (n.isAsk) open[n.who] = item.id else open.remove(n.who)
            }
            return open.values.toSet()
        }
    }
}

/** One question to another agent and its answer. [id] is the reply's notice id. */
data class PeerExchange(
    val id: String,
    val askId: String,
    val agent: AgentKind,
    val repo: String,
    val question: String,
    val answer: String,
) {
    companion object {
        /** An old reply, cut short ("…"), found whole among the helper's own messages. */
        fun full(answer: String, items: List<TimelineItem>): String? {
            if (!answer.endsWith("…")) return null
            val head = answer.dropLast(1).trim()
            if (head.length < 20) return null
            for (item in items.asReversed()) {
                val t = (item as? TimelineItem.Assistant)?.text?.trim() ?: continue
                if (t.length > head.length && t.startsWith(head)) return t
            }
            return null
        }
    }
}

/** A chat bubble: round, with a smaller corner at the bottom on the speaker's side. */
fun bubbleShape(mine: Boolean) = RoundedCornerShape(
    topStart = 18.dp, topEnd = 18.dp,
    bottomStart = if (mine) 18.dp else 6.dp, bottomEnd = if (mine) 6.dp else 18.dp,
)

/** Four dots taking turns, in the agent's colour. */
@Composable
fun WorkingDots(color: Color) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) { while (true) { delay(180); now = System.currentTimeMillis() } }
    val lit = ((now / 180) % 4).toInt()
    Box(Modifier.size(12.dp)) {
        listOf(0 to -4, 4 to 0, 0 to 4, -4 to 0).forEachIndexed { i, (x, y) ->
            Box(
                Modifier.align(Alignment.Center).offset(x = x.dp, y = y.dp).size(3.5.dp)
                    .background(color.copy(alpha = if (i == lit) 1f else 0.4f), CircleShape),
            )
        }
    }
}

/** The other agent's session for a note: one helping this session, else a live one. */
private fun helperFor(connection: DaemonConnection, agent: AgentKind, repo: String, sessionId: String): SessionSummary? =
    connection.sessions.firstOrNull { it.agent == agent && it.repoName == repo && it.helperOf == sessionId }
        ?: connection.sessions.firstOrNull { it.agent == agent && it.repoName == repo && it.status != SessionStatus.Done }

@Composable
private fun OpenLink(s: SessionSummary, c: Color, connection: DaemonConnection, label: String = "Open") {
    Row(
        Modifier.clickable { connection.pushedSession = s.id }.padding(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = c, style = MaterialTheme.typography.labelLarge)
        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, tint = c, modifier = Modifier.size(14.dp))
    }
}

/** Agents talking to each other, as cards. An ask: who asked whom, the question, and
 *  while it's out, what the other agent is doing. A reply without its ask: in the other
 *  agent's colour, folded when long. */
@Composable
fun PeerCard(note: PeerNote, asker: AgentKind, sessionId: String, waiting: Boolean, connection: DaemonConnection) {
    var expanded by remember { mutableStateOf(false) }
    val c = agentColor(note.agent)
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    val helper = helperFor(connection, note.agent, note.repo, sessionId)
    val codeBg = MaterialTheme.colorScheme.surfaceContainerHighest
    when (note.kind) {
        PeerNote.Kind.Asked, PeerNote.Kind.Started -> Column(
            Modifier.fillMaxWidth()
                .background(MaterialTheme.colorScheme.onSurface.copy(alpha = 0.04f), RoundedCornerShape(16.dp))
                .border(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.08f), RoundedCornerShape(16.dp))
                .padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                AgentAvatar(asker, 20.dp)
                Icon(Icons.AutoMirrored.Filled.ArrowForward, null, tint = grey, modifier = Modifier.size(12.dp))
                AgentAvatar(note.agent, 20.dp)
                Text(if (note.kind == PeerNote.Kind.Started) "Started ${note.agent.displayName} and asked" else "Asked ${note.agent.displayName}",
                    fontWeight = FontWeight.SemiBold, style = MaterialTheme.typography.bodySmall)
                Text(note.repo, color = grey, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Text(markdown(note.text, codeBg), color = grey, style = MaterialTheme.typography.bodyMedium,
                maxLines = if (expanded) Int.MAX_VALUE else 3, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.clickable { expanded = !expanded }.animateContentSize())
            if (waiting) Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                val name = note.agent.displayName
                when (helper?.status) {
                    SessionStatus.NeedsApproval -> {
                        Icon(Icons.Filled.PanTool, null, tint = attentionColor(), modifier = Modifier.size(14.dp))
                        Text("$name needs you", color = attentionColor(), style = MaterialTheme.typography.bodySmall)
                    }
                    SessionStatus.Working, null -> { WorkingDots(c); Shimmer("$name is on it…", c) }
                    else -> { WorkingDots(c); Text("Waiting for $name's reply", color = grey, style = MaterialTheme.typography.bodySmall) }
                }
                Spacer(Modifier.weight(1f))
                helper?.let { OpenLink(it, c, connection) }
            }
        }
        PeerNote.Kind.Replied -> {
            val long = note.text.length > 420
            Column(
                Modifier.fillMaxWidth().background(c.copy(alpha = 0.08f), RoundedCornerShape(18.dp))
                    .border(1.dp, c.copy(alpha = 0.3f), RoundedCornerShape(18.dp)).padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    AgentAvatar(note.agent, 24.dp)
                    Text("${note.agent.displayName} replied", color = c, fontWeight = FontWeight.SemiBold)
                    Text(note.repo, Modifier.weight(1f), color = grey, style = MaterialTheme.typography.bodySmall, maxLines = 1)
                    helper?.let { OpenLink(it, c, connection) }
                }
                SelectionContainer {
                    Text(markdown(note.text, codeBg), maxLines = if (long && !expanded) 8 else Int.MAX_VALUE,
                        overflow = TextOverflow.Ellipsis, modifier = Modifier.animateContentSize())
                }
                if (long) Text(if (expanded) "Show less" else "Show more", color = c, fontWeight = FontWeight.SemiBold,
                    style = MaterialTheme.typography.bodySmall, modifier = Modifier.clickable { expanded = !expanded })
            }
        }
        PeerNote.Kind.NoReply -> Text("No reply from ${note.agent.displayName} in time", color = grey, style = MaterialTheme.typography.bodySmall)
    }
}

/** An ask and its reply as one card, where the reply came: the question in the asking
 *  agent's bubble, the answer in the helper's. Tap for the whole thread between the two. */
@Composable
fun ExchangeCard(exchange: PeerExchange, asker: AgentKind, sessionId: String, thread: List<PeerExchange>, connection: DaemonConnection) {
    var open by remember { mutableStateOf(false) }
    val c = agentColor(exchange.agent)
    val mine = agentColor(asker)
    val grey = MaterialTheme.colorScheme.onSurfaceVariant
    val codeBg = MaterialTheme.colorScheme.surfaceContainerHighest
    val withThem = thread.filter { it.agent == exchange.agent && it.repo == exchange.repo }
    Column(
        Modifier.fillMaxWidth()
            .background(MaterialTheme.colorScheme.onSurface.copy(alpha = 0.04f), RoundedCornerShape(18.dp))
            .border(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.08f), RoundedCornerShape(18.dp))
            .clickable { open = true }
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            AgentAvatar(asker, 20.dp)
            Icon(Icons.Filled.SwapHoriz, null, tint = grey, modifier = Modifier.size(14.dp))
            AgentAvatar(exchange.agent, 20.dp)
            Text(
                androidx.compose.ui.text.buildAnnotatedString {
                    pushStyle(androidx.compose.ui.text.SpanStyle(fontWeight = FontWeight.SemiBold)); append(exchange.agent.displayName); pop()
                    pushStyle(androidx.compose.ui.text.SpanStyle(color = grey)); append(" · ${exchange.repo}"); pop()
                },
                Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            if (withThem.size > 1) Text("${withThem.size} exchanges", color = grey, style = MaterialTheme.typography.labelSmall, maxLines = 1)
            Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, "Show the whole conversation", tint = grey, modifier = Modifier.size(16.dp))
        }
        // The question: the asker's, on the right.
        Row(Modifier.fillMaxWidth()) {
            Spacer(Modifier.weight(1f).widthIn(min = 48.dp))
            Text(markdown(exchange.question, codeBg), style = MaterialTheme.typography.bodyMedium, maxLines = 3, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.background(mine.copy(alpha = 0.16f), bubbleShape(true)).padding(horizontal = 12.dp, vertical = 8.dp))
        }
        // The answer: the helper's, on the left.
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            AgentAvatar(exchange.agent, 22.dp)
            Text(markdown(exchange.answer, codeBg), style = MaterialTheme.typography.bodyMedium, maxLines = 6, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false).background(c.copy(alpha = 0.14f), bubbleShape(false)).padding(horizontal = 12.dp, vertical = 8.dp))
            Spacer(Modifier.width(24.dp))
        }
    }
    if (open) ExchangeThread(asker, exchange.agent, exchange.repo, withThem, sessionId, connection) { open = false }
}

/** Everything two agents said to each other, as a chat: the asking agent's messages on
 *  the right, the helper's on the left, in full and selectable. Opens at the latest. */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun ExchangeThread(
    asker: AgentKind,
    agent: AgentKind,
    repo: String,
    exchanges: List<PeerExchange>,
    sessionId: String,
    connection: DaemonConnection,
    onDismiss: () -> Unit,
) {
    val c = agentColor(agent)
    val mine = agentColor(asker)
    val codeBg = MaterialTheme.colorScheme.surfaceContainerHighest
    val helper = helperFor(connection, agent, repo, sessionId)
    // Replies from before they came whole were cut short: read them in the helper's own
    // conversation while this is open.
    val cut = exchanges.any { it.answer.endsWith("…") }
    if (cut && helper != null && helper.id != sessionId) DisposableEffect(helper.id) {
        connection.watch(helper.id)
        onDispose { connection.unwatch(helper.id) }
    }
    val helperItems = helper?.let { connection.timeline(it.id).items }
    androidx.compose.material3.ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = androidx.compose.material3.rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("${asker.displayName} and ${agent.displayName}", Modifier.weight(1f),
                    style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                if (helper != null) TextButton(onClick = { onDismiss(); connection.pushedSession = helper.id }) {
                    Text("Open ${agent.displayName}", color = c)
                }
            }
            val scroll = rememberScrollState()
            LaunchedEffect(Unit) { scroll.scrollTo(scroll.maxValue) }
            Column(Modifier.verticalScroll(scroll).padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                exchanges.forEach { x ->
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Spacer(Modifier.weight(1f).widthIn(min = 40.dp))
                        SelectionContainer(Modifier.weight(5f, fill = false)) {
                            Text(markdown(x.question, codeBg),
                                Modifier.background(mine.copy(alpha = 0.16f), bubbleShape(true)).padding(horizontal = 14.dp, vertical = 10.dp))
                        }
                        AgentAvatar(asker, 26.dp)
                    }
                    val answer = helperItems?.let { PeerExchange.full(x.answer, it) } ?: x.answer
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        AgentAvatar(agent, 26.dp)
                        SelectionContainer(Modifier.weight(5f, fill = false)) {
                            Text(markdown(answer, codeBg),
                                Modifier.background(c.copy(alpha = 0.14f), bubbleShape(false)).padding(horizontal = 14.dp, vertical = 10.dp))
                        }
                        Spacer(Modifier.weight(1f).widthIn(min = 40.dp))
                    }
                }
            }
        }
    }
}

/** Agents working together (`styr ask`): links from a session to the helpers it started,
 *  and from a helper back to the session it's helping, each with what it's doing now. */
@Composable
fun HelperLinks(summary: SessionSummary, connection: DaemonConnection) {
    val asker = summary.helperOf?.let { connection.session(it) }
    val helpers = connection.sessions.filter { it.helperOf == summary.id }
    if (asker == null && helpers.isEmpty()) return
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        asker?.let { HelperChip(it, "Helping ${it.agent.displayName} · ${it.repoName}", connection) }
        helpers.forEach { HelperChip(it, "${it.agent.displayName} is helping", connection) }
    }
}

@Composable
private fun HelperChip(s: SessionSummary, title: String, connection: DaemonConnection) {
    val c = agentColor(s.agent)
    Row(
        Modifier.background(c.copy(alpha = 0.12f), RoundedCornerShape(50)).clickable { connection.pushedSession = s.id }
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        AgentAvatar(s.agent, 20.dp)
        Text(title, style = MaterialTheme.typography.labelLarge, maxLines = 1)
        when (s.status) {
            SessionStatus.Working -> WorkingDots(c)
            SessionStatus.NeedsApproval -> Icon(Icons.Filled.PanTool, "Needs you", tint = attentionColor(), modifier = Modifier.size(14.dp))
            else -> Icon(Icons.Filled.Check, "Done", tint = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.size(12.dp))
        }
    }
}

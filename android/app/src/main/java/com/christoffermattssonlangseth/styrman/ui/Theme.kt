package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.DriveFileMove
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Extension
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.Checklist
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.SessionStatus
import com.christoffermattssonlangseth.styrman.ToolKind

/** Helm theme: ink, one signal-red accent, quiet greys (same as iOS). */
object Palette {
    val accent = Color(0xFFE8483F)
    val ink = Color(0xFF111214)
}

@Composable
fun StyrmanTheme(content: @Composable () -> Unit) {
    val scheme = if (isSystemInDarkTheme()) {
        darkColorScheme(primary = Palette.accent, secondary = Palette.accent)
    } else {
        lightColorScheme(primary = Palette.accent, secondary = Palette.accent)
    }
    MaterialTheme(colorScheme = scheme, content = content)
}

/** One colour per agent (light/dark variants match the iOS app). */
@Composable
fun agentColor(agent: AgentKind): Color {
    val dark = isSystemInDarkTheme()
    return when (agent) {
        AgentKind.Claude -> if (dark) Color(0xFFF0936B) else Color(0xFFB4532A)
        AgentKind.Antigravity -> if (dark) Color(0xFF7AA7FF) else Color(0xFF1F5FD1)
        AgentKind.Codex -> if (dark) Color(0xFF4CC79A) else Color(0xFF0F7B5A)
        else -> if (dark) Color(0xFFA6A9B0) else Color(0xFF5E6168)
    }
}

@Composable
fun statusColor(s: SessionStatus): Color = when (s) {
    SessionStatus.Working -> Color(0xFF2F7DF6)
    SessionStatus.NeedsApproval -> Palette.accent
    SessionStatus.Waiting -> Color(0xFFF29A2E)
    SessionStatus.Error -> Color(0xFFD93025)
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

fun toolIcon(k: ToolKind): ImageVector = when (k) {
    ToolKind.Execute -> Icons.Filled.Terminal
    ToolKind.Read -> Icons.Filled.Description
    ToolKind.Edit -> Icons.Filled.Edit
    ToolKind.Delete -> Icons.Filled.Delete
    ToolKind.Move -> Icons.Filled.DriveFileMove
    ToolKind.Search -> Icons.Filled.Search
    ToolKind.Fetch -> Icons.Filled.Language
    ToolKind.Think -> Icons.Filled.Checklist
    ToolKind.Mcp -> Icons.Filled.Extension
    ToolKind.Other -> Icons.Filled.Build
}

val Mono = FontFamily.Monospace

/** A filled circle in the agent's colour with its initial: tells agents apart at a glance. */
@Composable
fun AgentAvatar(agent: AgentKind, size: androidx.compose.ui.unit.Dp = 34.dp) {
    Box(
        Modifier.size(size).background(agentColor(agent), CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            when (agent) {
                AgentKind.Claude -> "✳"
                AgentKind.Codex -> "</>"
                AgentKind.Antigravity -> "⚛"
                else -> ">_"
            },
            color = Color.White,
            fontWeight = FontWeight.Bold,
            fontSize = (size.value * 0.36f).sp,
        )
    }
}

@Composable
fun AgentBadge(agent: AgentKind) {
    val c = agentColor(agent)
    Text(
        agent.displayName,
        color = c,
        fontSize = 11.sp,
        fontWeight = FontWeight.Bold,
        modifier = Modifier
            .background(c.copy(alpha = 0.16f), RoundedCornerShape(5.dp))
            .border(1.dp, c.copy(alpha = 0.35f), RoundedCornerShape(5.dp))
            .padding(horizontal = 7.dp, vertical = 3.dp),
    )
}

@Composable
fun StatusChip(status: SessionStatus) {
    val c = statusColor(status)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(5.dp),
        modifier = Modifier
            .background(c.copy(alpha = 0.12f), RoundedCornerShape(50))
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        Box(Modifier.size(7.dp).background(c, CircleShape))
        Text(status.label, fontSize = 12.sp, fontWeight = FontWeight.Medium)
    }
}

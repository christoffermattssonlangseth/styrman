package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.SessionStatus
import com.christoffermattssonlangseth.styrman.SessionSummary

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionListScreen(
    connection: DaemonConnection,
    onOpen: (String) -> Unit,
    onSettings: () -> Unit,
    onNewSession: () -> Unit,
) {
    val sessions = connection.sessions.toList()
    val groups = listOf(
        "Needs you" to sessions.filter { it.status in setOf(SessionStatus.NeedsApproval, SessionStatus.Waiting, SessionStatus.Error) },
        "Working" to sessions.filter { it.status == SessionStatus.Working },
        "Done" to sessions.filter { it.status == SessionStatus.Done || it.status == SessionStatus.Unknown },
    ).filter { it.second.isNotEmpty() }
    val connected = connection.state == DaemonConnection.State.Connected

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Styrman") },
                actions = {
                    IconButton(onClick = onNewSession, enabled = connected) { Icon(Icons.Filled.Add, "New session") }
                    IconButton(onClick = onSettings) { Icon(Icons.Filled.Settings, "Settings") }
                },
            )
        },
    ) { pad ->
        PullToRefreshBox(isRefreshing = false, onRefresh = { connection.refresh() }, modifier = Modifier.padding(pad)) {
            LazyColumn(Modifier.fillMaxSize()) {
                connectionBanner(connection)?.let { banner -> item { banner() } }
                connection.warnings.forEach { w -> item { Notice(w) } }
                if (sessions.isEmpty() && connected) {
                    item {
                        Column(Modifier.fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                            Text("No shared sessions", style = MaterialTheme.typography.titleMedium)
                            Text(
                                "Type /styr in Claude Code, Codex or agy on your Mac, or tap + to start one.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
                groups.forEach { (title, list) ->
                    item {
                        Text(
                            title,
                            style = MaterialTheme.typography.labelLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
                        )
                    }
                    items(list, key = { it.id }) { s -> SwipeRow(connection, s, onOpen) }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeRow(connection: DaemonConnection, s: SessionSummary, onOpen: (String) -> Unit) {
    val state = rememberSwipeToDismissBoxState(confirmValueChange = {
        if (it == SwipeToDismissBoxValue.EndToStart) { connection.stopSharing(s.id); true } else false
    })
    SwipeToDismissBox(
        state = state,
        enableDismissFromStartToEnd = false,
        backgroundContent = {
            Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.CenterEnd) {
                Text(if (s.status == SessionStatus.Done) "Remove" else "Stop sharing", color = MaterialTheme.colorScheme.error)
            }
        },
    ) {
        Column(
            Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.surface)
                .clickable { onOpen(s.id) }.padding(horizontal = 16.dp, vertical = 12.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                AgentBadge(s.agent)
                Spacer(Modifier.width(8.dp))
                Text(s.repoName, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                StatusChip(s.status)
            }
            s.title?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        HorizontalDivider()
    }
}

private fun connectionBanner(c: DaemonConnection): (@Composable () -> Unit)? = when (val s = c.state) {
    DaemonConnection.State.Connected -> null
    DaemonConnection.State.Connecting -> { { Notice("Connecting to ${c.host.ifEmpty { "your Mac" }}…") } }
    is DaemonConnection.State.Offline -> { { Notice("${s.why}. Is Tailscale on? Pull to retry.") } }
    DaemonConnection.State.Unauthorized -> { { Notice("This phone isn't paired with the Mac any more. Open Settings › Forget this Mac, then pair again.") } }
    DaemonConnection.State.Unpaired -> null
}

@Composable
fun Notice(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    )
}

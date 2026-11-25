package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Inventory2
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection

/** Start a session on the Mac: an agent and a folder in your home; it opens in a new cmux tab. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewSessionSheet(connection: DaemonConnection, onDismiss: () -> Unit) {
    val listing = connection.folders
    var agent by remember { mutableStateOf<AgentKind?>(null) }
    var folder by remember { mutableStateOf<String?>(null) }
    var first by remember { mutableStateOf("") }

    LaunchedEffect(Unit) { connection.listFolders(null) }
    LaunchedEffect(listing?.agents) { if (agent == null) agent = listing?.agents?.firstOrNull() }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.padding(horizontal = 16.dp).padding(bottom = 24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("New session", style = MaterialTheme.typography.titleLarge)
            val agents = listing?.agents.orEmpty()
            if (agents.isEmpty() && listing != null) Text("No agents are set up on the Mac. Run `styr setup` there.")
            if (agents.isNotEmpty()) SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                agents.forEachIndexed { i, a ->
                    SegmentedButton(selected = agent == a, onClick = { agent = a }, shape = SegmentedButtonDefaults.itemShape(i, agents.size)) {
                        Text(a.displayName)
                    }
                }
            }
            Text(
                folder?.let { "Folder: ${listing?.display(it) ?: it}" } ?: "Choose a folder",
                fontWeight = FontWeight.SemiBold,
                color = agent?.let { agentColor(it) } ?: MaterialTheme.colorScheme.onSurface,
            )
            if (listing != null) LazyColumn(Modifier.heightIn(max = 360.dp)) {
                if (listing.recent.isNotEmpty() && listing.path == listing.home) {
                    item { Label("Recent") }
                    items(listing.recent) { p -> FolderRow(listing.display(p), p, repo = true, selected = folder == p, onPick = { folder = it }, onOpen = null) }
                }
                item { Label(listing.display(listing.path)) }
                listing.parent?.let { parent ->
                    item {
                        Row(Modifier.fillMaxWidth().clickable { connection.listFolders(parent) }.padding(vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically) {
                            Icon(Icons.Filled.ArrowUpward, null, Modifier.size(20.dp))
                            Text("  Up to ${listing.display(parent)}")
                        }
                    }
                }
                item { FolderRow("Use ${listing.display(listing.path)}", listing.path, repo = false, selected = folder == listing.path, onPick = { folder = it }, onOpen = null) }
                items(listing.entries, key = { it.path }) { e ->
                    FolderRow(e.name, e.path, e.isRepo, folder == e.path, onPick = { folder = it }, onOpen = { connection.listFolders(it) })
                }
            }
            OutlinedTextField(first, { first = it }, label = { Text("First message (optional)") }, modifier = Modifier.fillMaxWidth(), maxLines = 4)
            Text(
                "Opens a new cmux tab on your Mac running ${agent?.displayName ?: "the agent"} in this folder, shared with this phone.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Button(
                onClick = {
                    val a = agent ?: return@Button
                    val f = folder ?: return@Button
                    if (connection.startSession(a, f, first)) onDismiss()
                },
                enabled = agent != null && folder != null && connection.state == DaemonConnection.State.Connected,
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Start") }
        }
    }
}

@Composable
private fun Label(text: String) = Text(
    text, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.padding(top = 8.dp, bottom = 4.dp),
)

@Composable
private fun FolderRow(name: String, path: String, repo: Boolean, selected: Boolean, onPick: (String) -> Unit, onOpen: ((String) -> Unit)?) {
    Row(Modifier.fillMaxWidth().clickable { onPick(path) }.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(
            if (selected) Icons.Filled.CheckCircle else if (repo) Icons.Filled.Inventory2 else Icons.Filled.Folder,
            null,
            Modifier.size(20.dp),
            tint = if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text("  $name", Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
        if (onOpen != null) IconButton(onClick = { onOpen(path) }) { Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, "Open $name") }
    }
}

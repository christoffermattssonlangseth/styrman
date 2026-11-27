package com.christoffermattssonlangseth.styrman.ui

import android.speech.tts.Voice
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.PlayCircle
import androidx.compose.material.icons.filled.StopCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.Speaker

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(connection: DaemonConnection, speaker: Speaker, onClose: () -> Unit) {
    var confirmForget by remember { mutableStateOf(false) }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text("Settings") },
            navigationIcon = { IconButton(onClick = onClose) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
        )
    }) { pad ->
        Column(
            Modifier.fillMaxSize().padding(pad).padding(16.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Mac", style = MaterialTheme.typography.titleMedium)
            Text("Name: ${connection.host.ifEmpty { "—" }}")
            Text("Status: " + when (val s = connection.state) {
                DaemonConnection.State.Connected ->
                    if (connection.overLocalNetwork) "Connected over Wi-Fi (Tailscale unreachable)" else "Connected"
                DaemonConnection.State.Connecting -> "Connecting"
                is DaemonConnection.State.Offline -> "Offline"
                DaemonConnection.State.Unauthorized -> "Not authorized"
                DaemonConnection.State.Unpaired -> "Not paired"
            })
            TextButton(onClick = { confirmForget = true }) { Text("Forget this Mac", color = MaterialTheme.colorScheme.error) }
            Text("To revoke this phone from the Mac side, run `styr devices revoke`.", style = MaterialTheme.typography.bodySmall)
            HorizontalDivider()

            Text("Voice", style = MaterialTheme.typography.titleMedium)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Read replies aloud", Modifier.weight(1f))
                Switch(checked = speaker.autoRead, onCheckedChange = { speaker.updateAutoRead(it) })
            }
            speaker.agents.forEach { agent -> VoiceRow(speaker, agent) }
            Text(
                "Each agent reads its replies in its own voice. For more natural voices, install high-quality " +
                    "voices in Android Settings › Accessibility › Text-to-speech output (e.g. Google's speech services).",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            HorizontalDivider()
            Text(
                "Notifications aren't available on Android yet: Styrman's Mac side sends them through Apple's " +
                    "push service. Keep the app open to follow along, or use an iPhone for pushes.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    if (confirmForget) AlertDialog(
        onDismissRequest = { confirmForget = false },
        title = { Text("Forget ${connection.host}?") },
        confirmButton = { TextButton(onClick = { connection.forget(); confirmForget = false; onClose() }) { Text("Forget") } },
        dismissButton = { TextButton(onClick = { confirmForget = false }) { Text("Cancel") } },
    )
}

@Composable
private fun VoiceRow(speaker: Speaker, agent: AgentKind) {
    var open by remember { mutableStateOf(false) }
    var version by remember { mutableIntStateOf(0) } // re-read the choice after picking
    val current = remember(version, speaker.voices) { speaker.voiceFor(agent) }
    val chosen = remember(version) { speaker.chosen(agent) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        AgentBadge(agent)
        Spacer(Modifier.width(8.dp))
        Column(Modifier.weight(1f)) {
            TextButton(onClick = { open = true }) {
                Text(
                    (if (chosen == null) "Automatic" else "Chosen") + (current?.let { " (${label(it)})" } ?: ""),
                    fontWeight = FontWeight.Normal,
                )
            }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                DropdownMenuItem(text = { Text("Automatic") }, onClick = { speaker.choose(null, agent); version++; open = false; speaker.preview(agent) })
                speaker.voices.forEach { v ->
                    DropdownMenuItem(text = { Text(label(v)) }, onClick = { speaker.choose(v.name, agent); version++; open = false; speaker.preview(agent) })
                }
            }
        }
        val previewing = speaker.speakingId == "preview-${agent.wire}"
        IconButton(onClick = { if (previewing) speaker.stop() else speaker.preview(agent) }) {
            Icon(if (previewing) Icons.Filled.StopCircle else Icons.Filled.PlayCircle, "Preview ${agent.displayName}'s voice", tint = agentColor(agent))
        }
    }
}

private fun label(v: Voice): String {
    val q = when {
        v.quality >= Voice.QUALITY_VERY_HIGH -> " · Very high"
        v.quality >= Voice.QUALITY_HIGH -> " · High"
        else -> ""
    }
    return "${v.name}$q"
}

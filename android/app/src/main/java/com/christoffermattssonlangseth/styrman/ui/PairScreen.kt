package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.Pairing
import com.christoffermattssonlangseth.styrman.PairingLink
import kotlinx.coroutines.launch

/**
 * Pair with a Mac: scan the QR code from `styr pair` with the camera (it opens this app),
 * or paste the `styrman://pair?…` link.
 */
@Composable
fun PairScreen(connection: DaemonConnection, incoming: PairingLink?, onDone: () -> Unit) {
    var text by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    fun pair(link: PairingLink) {
        busy = true
        error = null
        scope.launch {
            runCatching { Pairing.pair(link) }
                .onSuccess { connection.adopt(it); onDone() }
                .onFailure { error = it.message ?: "Couldn't reach the Mac. Is Tailscale on?" }
            busy = false
        }
    }

    LaunchedEffect(incoming) { incoming?.let { pair(it) } }

    Surface(Modifier.fillMaxSize()) {
        Column(
            Modifier.safeDrawingPadding().padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text("Styrman", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
            Text(
                "Pair this phone with your Mac. On the Mac, run `styr pair`, then scan the QR code " +
                    "with your camera app, or paste the link it shows below.",
                style = MaterialTheme.typography.bodyMedium,
            )
            OutlinedTextField(
                value = text,
                onValueChange = { text = it; error = null },
                label = { Text("styrman://pair?…") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = false,
            )
            Button(
                onClick = {
                    val link = PairingLink.parse(text)
                    if (link == null) error = "That isn't a Styrman pairing link." else pair(link)
                },
                enabled = !busy && text.isNotBlank(),
            ) { Text("Pair") }
            if (busy) CircularProgressIndicator()
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Text(
                "Your phone and Mac connect over Tailscale; nothing goes through a server.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

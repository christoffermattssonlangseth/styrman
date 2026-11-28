package com.christoffermattssonlangseth.styrman.ui

import androidx.activity.compose.BackHandler
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.PairingLink
import com.christoffermattssonlangseth.styrman.Speaker

/** Top-level navigation: pairing, the session list, a session, settings. */
@Composable
fun StyrmanApp(connection: DaemonConnection, speaker: Speaker, incomingLink: PairingLink?, consumeLink: () -> Unit) {
    // The open sessions, the top one shown; saved as one string so it survives rotation.
    var stack by rememberSaveable { mutableStateOf("") }
    val open = stack.split('\n').lastOrNull { it.isNotEmpty() }
    fun back() { stack = stack.substringBeforeLast('\n', "") }
    var usage by rememberSaveable { mutableStateOf(false) }
    var settings by remember { mutableStateOf(false) }
    var newSession by remember { mutableStateOf(false) }

    // A session started from this phone opens as soon as the Mac answers.
    LaunchedEffect(connection.requestedSession) {
        connection.requestedSession?.let { stack = it; connection.requestedSession = null }
    }
    LaunchedEffect(connection.pushedSession) {
        connection.pushedSession?.let { if (it != open) stack = if (stack.isEmpty()) it else stack + "\n" + it }
        connection.pushedSession = null
    }

    val paired = connection.credentials != null
    when {
        !paired || incomingLink != null -> PairScreen(connection, incomingLink, onDone = consumeLink)
        usage -> {
            BackHandler { usage = false }
            UsageScreen(connection, onClose = { usage = false })
        }
        settings -> {
            BackHandler { settings = false }
            SettingsScreen(connection, speaker, onClose = { settings = false })
        }
        open != null -> {
            BackHandler { back() }
            // Keyed: opening a helper from a session starts its screen afresh.
            androidx.compose.runtime.key(open) { SessionScreen(connection, speaker, open, onBack = { back() }) }
        }
        else -> SessionListScreen(
            connection,
            onOpen = { stack = it },
            onUsage = { usage = true },
            onSettings = { settings = true },
            onNewSession = { newSession = true },
        )
    }
    if (newSession) NewSessionSheet(connection, onDismiss = { newSession = false })
}

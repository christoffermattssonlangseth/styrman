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
    var open by rememberSaveable { mutableStateOf<String?>(null) }
    var settings by remember { mutableStateOf(false) }
    var newSession by remember { mutableStateOf(false) }

    // A session started from this phone opens as soon as the Mac answers.
    LaunchedEffect(connection.requestedSession) {
        connection.requestedSession?.let { open = it; connection.requestedSession = null }
    }

    val paired = connection.credentials != null
    when {
        !paired || incomingLink != null -> PairScreen(connection, incomingLink, onDone = consumeLink)
        settings -> {
            BackHandler { settings = false }
            SettingsScreen(connection, speaker, onClose = { settings = false })
        }
        open != null -> {
            BackHandler { open = null }
            SessionScreen(connection, speaker, open!!, onBack = { open = null })
        }
        else -> SessionListScreen(
            connection,
            onOpen = { open = it },
            onSettings = { settings = true },
            onNewSession = { newSession = true },
        )
    }
    if (newSession) NewSessionSheet(connection, onDismiss = { newSession = false })
}

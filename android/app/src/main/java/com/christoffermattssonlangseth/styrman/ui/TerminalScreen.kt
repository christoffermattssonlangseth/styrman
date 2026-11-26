package com.christoffermattssonlangseth.styrman.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.ZoomIn
import androidx.compose.material.icons.filled.ZoomOut
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.christoffermattssonlangseth.styrman.AgentKind
import com.christoffermattssonlangseth.styrman.DaemonConnection
import kotlinx.coroutines.delay

private val TerminalBackground = Color(0xFF121214)
private val TerminalText = Color(0xFFE6E6E6)

/** The agent's terminal on the Mac, live and read-only: what `styr` sees on screen. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TerminalDialog(connection: DaemonConnection, sessionId: String, agent: AgentKind, onDismiss: () -> Unit) {
    val screen = connection.screens[sessionId]
    var zoom by remember { mutableFloatStateOf(1f) }

    // Keep asking while it's open: the Mac stops sending 20 s after the last ask.
    LaunchedEffect(sessionId) {
        while (true) {
            connection.watchScreen(sessionId)
            delay(8_000)
        }
    }
    // A screen from an earlier look would pass for the current one.
    DisposableEffect(sessionId) { onDispose { connection.screens.remove(sessionId) } }

    FullScreenDialog(onDismiss) {
        Scaffold(
            containerColor = TerminalBackground,
            topBar = {
                TopAppBar(
                    title = { Text("Terminal") },
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = TerminalBackground,
                        titleContentColor = TerminalText,
                        navigationIconContentColor = TerminalText,
                        actionIconContentColor = TerminalText,
                    ),
                    navigationIcon = { IconButton(onClick = onDismiss) { Icon(Icons.Filled.Close, "Close") } },
                    actions = {
                        IconButton(onClick = { zoom = (zoom / 1.25f).coerceAtLeast(0.6f) }) { Icon(Icons.Filled.ZoomOut, "Smaller text") }
                        IconButton(onClick = { zoom = (zoom * 1.25f).coerceAtMost(4f) }) { Icon(Icons.Filled.ZoomIn, "Larger text") }
                    },
                )
            },
        ) { pad ->
            BoxWithConstraints(Modifier.fillMaxSize().padding(pad).background(TerminalBackground)) {
                if (screen == null) {
                    Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally) {
                        CircularProgressIndicator(color = agentColor(agent))
                        Spacer(Modifier.size(12.dp))
                        Text("Asking the Mac for the screen…", color = TerminalText)
                    }
                } else {
                    // Sized to fit the width at zoom 1: a monospaced character is about 0.6 of the font size wide.
                    val cols = maxOf(screen.cols, 20)
                    val fit = with(LocalDensity.current) { ((maxWidth - 16.dp) / (cols * 0.6f)).toSp() }
                    ScreenText(screen.text, (maxOf(fit.value, 5f) * zoom).sp)
                }
            }
        }
    }
}

@Composable
private fun ScreenText(text: String, size: androidx.compose.ui.unit.TextUnit) {
    val vertical = rememberScrollState()
    var placed by remember { mutableStateOf(false) }
    // Open at the bottom, where the prompt and the latest output are.
    LaunchedEffect(vertical.maxValue) {
        if (!placed && vertical.maxValue > 0) { vertical.scrollTo(vertical.maxValue); placed = true }
    }
    Box(Modifier.fillMaxSize().verticalScroll(vertical).horizontalScroll(rememberScrollState())) {
        SelectionContainer {
            Text(
                text.trim('\n'),
                color = TerminalText,
                fontFamily = Mono,
                fontSize = size,
                lineHeight = size * 1.2f,
                softWrap = false,
                modifier = Modifier.padding(8.dp),
            )
        }
    }
}

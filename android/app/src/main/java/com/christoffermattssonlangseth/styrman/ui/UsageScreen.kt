package com.christoffermattssonlangseth.styrman.ui

import android.text.format.DateUtils
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.christoffermattssonlangseth.styrman.DaemonConnection
import com.christoffermattssonlangseth.styrman.UsageProvider
import java.text.NumberFormat
import kotlinx.coroutines.delay

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun UsageScreen(connection: DaemonConnection, onClose: () -> Unit) {
    val connected = connection.state == DaemonConnection.State.Connected
    val providers = connection.usage
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(connected) {
        if (connected) {
            while (true) {
                connection.refreshUsage()
                delay(60_000)
            }
        }
    }
    LaunchedEffect(Unit) {
        while (true) {
            now = System.currentTimeMillis()
            delay(30_000)
        }
    }

    Scaffold(topBar = {
        TopAppBar(
            title = { Text("Usage") },
            navigationIcon = { IconButton(onClick = onClose) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            actions = {
                IconButton(onClick = connection::refreshUsage, enabled = connected && connection.usageSupported) {
                    Icon(Icons.Filled.Refresh, "Refresh usage")
                }
            },
        )
    }) { pad ->
        LazyColumn(
            modifier = Modifier.padding(pad),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (!connected) item { Notice("Disconnected from Mac") }
            if (!providers.isNullOrEmpty()) {
                items(providers, key = { it.id }) { provider -> UsageCard(provider, now) }
            } else if (connected) {
                item {
                    when {
                        !connection.usageSupported -> Notice("Update Styrman on your Mac to view usage.")
                        providers == null -> Notice("Loading usage…")
                        else -> Notice("No usage available. Open Karv on your Mac and sync it.")
                    }
                }
            }
        }
    }
}

@Composable
private fun UsageCard(provider: UsageProvider, now: Long) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(provider.name, style = MaterialTheme.typography.titleMedium)
            for (window in provider.windows) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                    Box(Modifier.size(60.dp), contentAlignment = Alignment.Center) {
                        CircularProgressIndicator(
                            progress = { window.usedFraction.toFloat() },
                            modifier = Modifier.size(56.dp),
                        )
                        Text(NumberFormat.getPercentInstance().format(window.usedFraction), style = MaterialTheme.typography.labelLarge)
                    }
                    Column {
                        Text(window.title)
                        val status = if (window.resetsAtMs > now) {
                            val relative = DateUtils.getRelativeTimeSpanString(window.resetsAtMs, now, DateUtils.MINUTE_IN_MILLIS)
                            if (window.rolling) "Next relief $relative" else "Resets $relative"
                        } else {
                            "Awaiting a fresh reading"
                        }
                        Text(status, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            val source = if (provider.estimated) "Activity estimate" else "Reported quota"
            val stale = if (now - provider.observedAtMs > 180_000) " · Stale" else ""
            val observed = DateUtils.getRelativeTimeSpanString(provider.observedAtMs, now, DateUtils.MINUTE_IN_MILLIS)
            Text("$source$stale · Updated $observed", style = MaterialTheme.typography.bodySmall)
        }
    }
}

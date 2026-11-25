package com.christoffermattssonlangseth.styrman

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.mutableStateOf
import com.christoffermattssonlangseth.styrman.ui.StyrmanApp
import com.christoffermattssonlangseth.styrman.ui.StyrmanTheme

class MainActivity : ComponentActivity() {
    private lateinit var connection: DaemonConnection
    private lateinit var speaker: Speaker
    /** A pairing link the app was opened with (scanned QR code). */
    private val incomingLink = mutableStateOf<PairingLink?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        connection = DaemonConnection(applicationContext)
        speaker = Speaker(applicationContext)
        handle(intent)
        setContent {
            StyrmanTheme {
                StyrmanApp(connection, speaker, incomingLink.value) { incomingLink.value = null }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(intent: Intent?) {
        intent?.data?.asPairingLink()?.let { incomingLink.value = it }
    }

    // Connected while visible, like the iOS app (no background socket).
    override fun onStart() {
        super.onStart()
        connection.start()
    }

    override fun onStop() {
        super.onStop()
        connection.stop()
    }

    override fun onDestroy() {
        speaker.shutdown()
        super.onDestroy()
    }
}

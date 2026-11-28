package com.christoffermattssonlangseth.styrman

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.serialization.json.JsonObject
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

/** The connection to styrd's phone API, and everything it knows (Compose state). */
class DaemonConnection(context: Context) {
    sealed interface State {
        data object Unpaired : State
        data object Connecting : State
        data object Connected : State
        data class Offline(val why: String) : State
        data object Unauthorized : State
    }

    private val store = CredentialStore(context)
    private val main = Handler(Looper.getMainLooper())
    private val http = OkHttpClient.Builder()
        .pingInterval(20, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .build()

    var credentials: Credentials? by mutableStateOf(store.load())
        private set
    var state: State by mutableStateOf(if (credentials == null) State.Unpaired else State.Connecting)
        private set
    val sessions = mutableStateListOf<SessionSummary>()
    var host by mutableStateOf(credentials?.host ?: "")
        private set
    var usageSupported by mutableStateOf(false)
        private set
    var usage by mutableStateOf<List<UsageProvider>?>(null)
        private set
    var warnings by mutableStateOf(listOf<String>())
        private set
    /** Last error the daemon reported for something this phone asked (shown briefly). */
    var lastError: String? by mutableStateOf(null)
    /** A session to open (just started from the phone). */
    var requestedSession: String? by mutableStateOf(null)
    /** Opened from inside a session (a helper's chip or chat): goes on top, Back returns. */
    var pushedSession: String? by mutableStateOf(null)
    var folders: FolderListing? by mutableStateOf(null)
        private set
    /** True while connected over the local-network fallback (Tailscale unreachable). */
    var overLocalNetwork by mutableStateOf(false)
        private set

    /** A session folder's uncommitted changes, or why they couldn't be read. */
    data class Changes(val branch: String?, val files: List<FileDiff>, val truncated: Boolean, val error: String? = null)

    /** Uncommitted changes asked for, per session. */
    val changes = mutableStateMapOf<String, Changes>()
    /** Terminal screens being watched, per session. */
    val screens = mutableStateMapOf<String, ServerMsg.Screen>()
    // The daemon's errors name no session: one arriving while changes are awaited is theirs.
    private var awaitingChanges: String? = null

    private val timelines = HashMap<String, SessionTimeline>()
    private val watched = HashSet<String>()
    private var ws: WebSocket? = null
    private var generation = 0
    private var backoffMs = 1000L
    private var wanted = false
    private var startPrompt: String? = null

    fun timeline(id: String) = timelines.getOrPut(id) { SessionTimeline(id) }
    fun session(id: String) = sessions.firstOrNull { it.id == id }

    // ---- lifecycle ----

    fun start() {
        wanted = true
        connect()
    }

    fun stop() {
        wanted = false
        generation++
        ws?.close(1000, null)
        ws = null
        timelines.values.forEach { it.isLive = false }
        if (credentials != null && state != State.Unauthorized) state = State.Offline("Disconnected")
    }

    fun adopt(c: Credentials) {
        usage = null
        usageSupported = false
        store.save(c)
        credentials = c
        host = c.host
        backoffMs = 1000
        stop()
        start()
    }

    fun forget() {
        stop()
        usage = null
        usageSupported = false
        store.clear()
        credentials = null
        sessions.clear()
        timelines.clear()
        changes.clear()
        screens.clear()
        state = State.Unpaired
    }

    /** [lanIndex] < 0: the Tailscale URL; otherwise that local-network fallback URL. */
    private fun connect(lanIndex: Int = -1) {
        val c = credentials ?: run { state = State.Unpaired; return }
        if (!wanted) return
        val gen = ++generation
        state = State.Connecting
        val lan = c.lan
        val (url, client) = if (lanIndex < 0 || lan == null) c.wsUrl to http
            else lan.urls[lanIndex] to pinnedClient(lan)
        overLocalNetwork = lanIndex >= 0
        val req = Request.Builder().url(url).header("Authorization", "Bearer ${c.token}").build()
        ws = client.newWebSocket(req, object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                val msg = runCatching { ServerMsg.parse(text) }.getOrNull() ?: return
                main.post { if (gen == generation) handle(msg) }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                main.post { if (gen == generation) lost(response?.code == 401, t.message ?: "connection lost", lanIndex) }
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                main.post { if (gen == generation) lost(false, "connection closed", lanIndex) }
            }
        })
    }

    private var pinned: Pair<LanRoute, OkHttpClient>? = null

    /** A client that trusts only the Mac's local-network certificate (pinned by SHA-256). */
    private fun pinnedClient(lan: LanRoute): OkHttpClient {
        pinned?.let { (r, cl) -> if (r == lan) return cl }
        val tm = PinnedTrust(lan.cert)
        val ssl = SSLContext.getInstance("TLS").apply { init(null, arrayOf(tm), null) }
        val hosts = lan.urls.map { it.removePrefix("wss://").substringBefore('/').substringBeforeLast(':') }.toSet()
        val cl = http.newBuilder()
            .connectTimeout(4, TimeUnit.SECONDS)
            .sslSocketFactory(ssl.socketFactory, tm)
            // The certificate is pinned; it names no address, so check the host is one we were given.
            .hostnameVerifier { host, _ -> host in hosts }
            .build()
        pinned = lan to cl
        return cl
    }

    private fun lost(unauthorized: Boolean, why: String, lanIndex: Int = -1) {
        val wasConnected = state == State.Connected
        overLocalNetwork = false
        timelines.values.forEach { it.isLive = false }
        awaitingChanges?.let { changes[it] = Changes(null, emptyList(), false, error = "The connection to the Mac was lost.") }
        awaitingChanges = null
        if (unauthorized) {
            state = State.Unauthorized
            return
        }
        // Tailscale unreachable: try the Mac on the local network before waiting.
        val lan = credentials?.lan
        if (!wasConnected && wanted && lan != null && lanIndex + 1 < lan.urls.size) {
            connect(lanIndex + 1)
            return
        }
        state = State.Offline(if (wasConnected) "Connection lost" else "Can't reach ${host.ifEmpty { "the Mac" }}")
        if (!wanted) return
        val wait = backoffMs
        backoffMs = (backoffMs * 2).coerceAtMost(30_000)
        main.postDelayed({ if (wanted && state !is State.Connected) connect() }, wait)
    }

    /** Keep the Mac's local-network route (it changes with its Wi-Fi address). */
    private fun remember(lan: LanRoute?) {
        val c = credentials ?: return
        if (lan == null || (c.lanUrls == lan.urls && c.lanCert == lan.cert)) return
        val updated = c.copy(lanUrls = lan.urls, lanCert = lan.cert)
        store.save(updated)
        credentials = updated
    }

    private fun send(o: JsonObject): Boolean = ws?.send(o.toString()) == true

    // ---- messages ----

    private fun handle(msg: ServerMsg) {
        when (msg) {
            is ServerMsg.Hello -> {
                remember(msg.lan)
                host = msg.host
                warnings = msg.warnings
                usageSupported = msg.usageSupported
                state = State.Connected
                backoffMs = 1000
                watched.forEach { subscribe(it) }
            }
            is ServerMsg.Usage -> usage = msg.providers
            is ServerMsg.Sessions -> {
                sessions.clear()
                sessions.addAll(msg.sessions.sortedByDescending { it.updatedMs })
            }
            is ServerMsg.SessionUpdated -> {
                sessions.removeAll { it.id == msg.session.id }
                sessions.add(msg.session)
                sessions.sortByDescending { it.updatedMs }
            }
            is ServerMsg.SessionRemoved -> sessions.removeAll { it.id == msg.id }
            is ServerMsg.Events -> timelines[msg.session]?.apply(msg.events)
            is ServerMsg.Subscribed -> timelines[msg.session]?.isLive = true
            is ServerMsg.EventMsg -> timelines[msg.event.session]?.apply(listOf(msg.event))
            is ServerMsg.PromptQueued -> timelines[msg.session]?.markQueued(msg.text)
            is ServerMsg.PromptDelivered -> timelines[msg.session]?.markDelivered(msg.text)
            is ServerMsg.Folders -> folders = msg.listing
            is ServerMsg.SessionStarting -> {
                startPrompt?.let { timeline(msg.session).addPending(it) }
                startPrompt = null
                requestedSession = msg.session
                send(ClientMsg.listSessions())
            }
            is ServerMsg.Changes -> {
                if (awaitingChanges == msg.session) awaitingChanges = null
                changes[msg.session] = Changes(msg.branch, GitChanges.files(msg.diff), msg.truncated)
            }
            is ServerMsg.Screen -> screens[msg.session] = msg
            is ServerMsg.Preview -> timelines[msg.session]?.let {
                it.showPreview(msg.text)
                it.showStatus(msg.status)
            }
            is ServerMsg.Error -> {
                val waiting = awaitingChanges
                if (waiting != null) {
                    awaitingChanges = null
                    changes[waiting] = Changes(null, emptyList(), false, error = msg.message)
                } else lastError = msg.message
            }
            is ServerMsg.Unknown -> {}
        }
    }

    // ---- actions ----

    fun watch(id: String) {
        watched += id
        if (state == State.Connected) subscribe(id)
    }

    fun unwatch(id: String) {
        watched -= id
        if (state == State.Connected) send(ClientMsg.unsubscribe(id))
    }

    private fun subscribe(id: String) = send(ClientMsg.subscribe(id, timeline(id).lastSeq))

    fun refreshUsage() {
        if (state == State.Connected && usageSupported) send(ClientMsg.getUsage())
    }

    fun refresh() {
        if (state == State.Connected) send(ClientMsg.listSessions()) else { backoffMs = 1000; connect() }
    }

    fun resolvePermission(session: String, request: String, option: String): Boolean {
        if (state != State.Connected) return false
        return send(ClientMsg.resolvePermission(session, request, option))
    }

    fun sendPrompt(session: String, text: String): Boolean {
        val t = text.trim()
        if (state != State.Connected || t.isEmpty()) return false
        if (!send(ClientMsg.sendPrompt(session, t))) {
            lastError = "The connection closed before the message could be sent."
            return false
        }
        val timeline = timeline(session)
        val id = timeline.addPending(t)
        main.postDelayed({ timeline.markUnconfirmed(id) }, 15_000)
        return true
    }

    /** Stop the agent's current turn (Esc in its terminal). */
    fun interrupt(session: String) {
        if (state == State.Connected) send(ClientMsg.interrupt(session))
    }

    /** Quit the agent on the Mac (closing the tab if the phone opened it) and drop it from the list. */
    fun endSession(session: String) {
        if (state != State.Connected) return
        sessions.removeAll { it.id == session }
        send(ClientMsg.endSession(session))
    }

    /** Switch the permission mode. The Mac presses Shift+Tab until the footer shows it; if
     *  the session's mode hasn't followed after a few seconds, say so. */
    fun setMode(session: String, mode: PermissionMode) {
        if (state != State.Connected) return
        send(ClientMsg.setMode(session, mode.wire))
        main.postDelayed({
            val s = this.session(session) ?: return@postDelayed
            if (s.mode != mode.wire && lastError == null) {
                lastError = "${s.agent.displayName} didn't switch to ${mode.label}. " +
                    "This version may not have that mode, or a dialog is open in its terminal."
            }
        }, 6_000)
    }

    /** Ask for the session folder's uncommitted changes (they arrive in `changes`). */
    fun getChanges(session: String) {
        if (state != State.Connected) {
            changes[session] = Changes(null, emptyList(), false, error = "Not connected to the Mac.")
            return
        }
        changes.remove(session)
        awaitingChanges = session
        send(ClientMsg.getChanges(session))
    }

    /** Ask for the terminal screen for the next 20 s (call again to keep it). */
    fun watchScreen(session: String) {
        if (state == State.Connected) send(ClientMsg.watchScreen(session))
    }

    fun stopSharing(session: String) {
        if (state != State.Connected) return
        sessions.removeAll { it.id == session }
        send(ClientMsg.stopSharing(session))
    }

    fun createFolder(parent: String, name: String) {
        if (state == State.Connected) send(ClientMsg.createFolder(parent, name))
    }

    fun listFolders(path: String? = null) {
        if (state == State.Connected) send(ClientMsg.listFolders(path))
    }

    fun startSession(agent: AgentKind, cwd: String, prompt: String?): Boolean {
        if (state != State.Connected) return false
        startPrompt = prompt?.trim()?.takeIf { it.isNotEmpty() }
        send(ClientMsg.startSession(agent, cwd, startPrompt))
        return true
    }
}

/** Trusts exactly one certificate: the one whose SHA-256 (DER, lowercase hex) is [pin]. */
class PinnedTrust(private val pin: String) : X509TrustManager {
    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
        val leaf = chain?.firstOrNull() ?: throw CertificateException("no certificate")
        if (fingerprint(leaf.encoded) != pin) throw CertificateException("not the Mac's certificate")
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) =
        throw CertificateException("not a server")

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()

    companion object {
        fun fingerprint(der: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(der).joinToString("") { "%02x".format(it) }
    }
}

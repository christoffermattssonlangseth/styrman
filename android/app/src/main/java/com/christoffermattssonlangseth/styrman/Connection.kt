package com.christoffermattssonlangseth.styrman

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.serialization.json.JsonObject
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit

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
    var warnings by mutableStateOf(listOf<String>())
        private set
    /** Last error the daemon reported for something this phone asked (shown briefly). */
    var lastError: String? by mutableStateOf(null)
    /** A session to open (just started from the phone). */
    var requestedSession: String? by mutableStateOf(null)
    var folders: FolderListing? by mutableStateOf(null)
        private set

    private val timelines = HashMap<String, SessionTimeline>()
    private val watched = HashSet<String>()
    private var ws: WebSocket? = null
    private var generation = 0
    private var backoffMs = 1000L
    private var wanted = false
    private var startPrompt: String? = null
    private var lastSent: Triple<String, String, Long>? = null

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
    }

    fun adopt(c: Credentials) {
        store.save(c)
        credentials = c
        host = c.host
        backoffMs = 1000
        stop()
        start()
    }

    fun forget() {
        stop()
        store.clear()
        credentials = null
        sessions.clear()
        timelines.clear()
        state = State.Unpaired
    }

    private fun connect() {
        val c = credentials ?: run { state = State.Unpaired; return }
        if (!wanted) return
        val gen = ++generation
        state = State.Connecting
        val req = Request.Builder().url(c.wsUrl).header("Authorization", "Bearer ${c.token}").build()
        ws = http.newWebSocket(req, object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                val msg = runCatching { ServerMsg.parse(text) }.getOrNull() ?: return
                main.post { if (gen == generation) handle(msg) }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                main.post { if (gen == generation) lost(response?.code == 401, t.message ?: "connection lost") }
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                main.post { if (gen == generation) lost(false, "connection closed") }
            }
        })
    }

    private fun lost(unauthorized: Boolean, why: String) {
        timelines.values.forEach { it.isLive = false }
        if (unauthorized) {
            state = State.Unauthorized
            return
        }
        state = State.Offline(if (state == State.Connected) "Connection lost" else "Can't reach ${host.ifEmpty { "the Mac" }}")
        if (!wanted) return
        val wait = backoffMs
        backoffMs = (backoffMs * 2).coerceAtMost(30_000)
        main.postDelayed({ if (wanted && state !is State.Connected) connect() }, wait)
    }

    private fun send(o: JsonObject) {
        ws?.send(o.toString())
    }

    // ---- messages ----

    private fun handle(msg: ServerMsg) {
        when (msg) {
            is ServerMsg.Hello -> {
                host = msg.host
                warnings = msg.warnings
                state = State.Connected
                backoffMs = 1000
                watched.forEach { subscribe(it) }
            }
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
            is ServerMsg.PromptDelivered -> {
                timelines[msg.session]?.markDelivered(msg.text)
                if (lastSent?.second == msg.text) lastSent = null
            }
            is ServerMsg.Folders -> folders = msg.listing
            is ServerMsg.SessionStarting -> {
                startPrompt?.let { timeline(msg.session).addPending(it) }
                startPrompt = null
                requestedSession = msg.session
                send(ClientMsg.listSessions())
            }
            is ServerMsg.Error -> {
                lastError = msg.message
                // A refused prompt shouldn't sit in the timeline as "queued" forever.
                lastSent?.let { (sid, text, at) ->
                    if (System.currentTimeMillis() - at < 10_000) timelines[sid]?.dropPending(text)
                }
                lastSent = null
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

    fun refresh() {
        if (state == State.Connected) send(ClientMsg.listSessions()) else { backoffMs = 1000; connect() }
    }

    fun resolvePermission(session: String, request: String, option: String): Boolean {
        if (state != State.Connected) return false
        send(ClientMsg.resolvePermission(session, request, option))
        return true
    }

    fun sendPrompt(session: String, text: String): Boolean {
        val t = text.trim()
        if (state != State.Connected || t.isEmpty()) return false
        timeline(session).addPending(t)
        lastSent = Triple(session, t, System.currentTimeMillis())
        send(ClientMsg.sendPrompt(session, t))
        return true
    }

    fun stopSharing(session: String) {
        if (state != State.Connected) return
        sessions.removeAll { it.id == session }
        send(ClientMsg.stopSharing(session))
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

// Kotlin mirror of the styrd phone protocol (crates/styr-core/src/{event,ipc,phone}.rs).
// Field names are the Rust serde names. Decoding is tolerant like the iOS app's: unknown
// event or message types become `Unknown`, so an older app keeps working with a newer daemon.

package com.christoffermattssonlangseth.styrman

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

val json = Json { ignoreUnknownKeys = true; prettyPrint = true }

// ---- small helpers over JsonObject ----

fun JsonObject.str(key: String): String? = (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content
fun JsonObject.bool(key: String): Boolean? = (this[key] as? JsonPrimitive)?.booleanOrNull
fun JsonObject.long(key: String): Long? = (this[key] as? JsonPrimitive)?.longOrNull
fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject
fun JsonObject.arr(key: String): JsonArray? = this[key] as? JsonArray

/** Compact, human-readable rendering of a JSON value (strings as is). */
fun JsonElement.pretty(): String = when (this) {
    is JsonPrimitive -> if (isString) content else toString()
    JsonNull -> "null"
    else -> json.encodeToString(JsonElement.serializer(), this)
}

// ---- sessions ----

enum class AgentKind(val wire: String, val displayName: String) {
    Claude("claude", "Claude"),
    Antigravity("antigravity", "agy"),
    Codex("codex", "Codex"),
    Pty("pty", "Shell"),
    Unknown("unknown", "Agent");

    companion object {
        fun of(s: String?) = entries.firstOrNull { it.wire == s } ?: Unknown
    }
}

enum class SessionStatus(val wire: String, val label: String) {
    Working("working", "Working"),
    NeedsApproval("needs_approval", "Needs approval"),
    Waiting("waiting", "Needs you"),
    Error("error", "Error"),
    Done("done", "Done"),
    Unknown("unknown", "—");

    companion object {
        fun of(s: String?) = entries.firstOrNull { it.wire == s } ?: Unknown
    }
}

data class SessionSummary(
    val id: String,
    val agent: AgentKind,
    val nativeId: String,
    val cwd: String?,
    val title: String?,
    val model: String?,
    val shared: Boolean,
    val status: SessionStatus,
    val createdMs: Long,
    val updatedMs: Long,
    val lastSeq: Long,
    val inTerminalWrapper: Boolean,
    val acceptsPrompts: Boolean,
    /** Permission mode as the terminal shows it; known only inside `styr <agent>`. */
    val mode: String? = null,
    /** Its wrapper predates a Styrman update and lacks stop / terminal. */
    val outdated: Boolean = false,
    /** The session this one was started to help (`styr ask`), if any. */
    val helperOf: String? = null,
) {
    /** Quit the agent from the phone: needs the wrapper. */
    val canEnd get() = inTerminalWrapper && status != SessionStatus.Done

    /** Stop a turn and show the terminal: needs a current wrapper. */
    val hasLiveControls get() = inTerminalWrapper && !outdated

    val permissionMode get() = PermissionMode.of(mode)

    /** Repository / folder name for the list. */
    val repoName: String
        get() = cwd?.takeIf { it.isNotEmpty() }?.trimEnd('/')?.substringAfterLast('/') ?: (title ?: "Session")

    companion object {
        fun parse(o: JsonObject) = SessionSummary(
            id = o.str("id") ?: "",
            agent = AgentKind.of(o.str("agent")),
            nativeId = o.str("native_id") ?: "",
            cwd = o.str("cwd"),
            title = o.str("title"),
            model = o.str("model"),
            shared = o.bool("shared") ?: false,
            status = SessionStatus.of(o.str("status")),
            createdMs = o.long("created_ms") ?: 0,
            updatedMs = o.long("updated_ms") ?: 0,
            lastSeq = o.long("last_seq") ?: 0,
            inTerminalWrapper = o.bool("in_terminal_wrapper") ?: false,
            // Older daemons don't say; assume yes rather than hide the message box.
            acceptsPrompts = o.bool("accepts_prompts") ?: true,
            mode = o.str("mode"),
            outdated = o.bool("outdated") ?: false,
            helperOf = o.str("helper_of"),
        )
    }
}

/** Claude's permission modes (Shift+Tab in its terminal), labelled as on iOS. */
enum class PermissionMode(val wire: String, val label: String, val short: String, val detail: String) {
    Manual("default", "Manual", "Manual", "Asks before edits and commands"),
    AcceptEdits("acceptEdits", "Accept edits", "Edits", "Edits files without asking"),
    Plan("plan", "Plan", "Plan", "Plans first, changes nothing"),
    Auto("auto", "Auto", "Auto", "Decides for itself what needs asking"),
    BypassPermissions("bypassPermissions", "Bypass permissions", "Bypass", "Never asks");

    companion object {
        fun of(s: String?) = entries.firstOrNull { it.wire == s }

        /** What the phone offers: bypass only when the session already allows it. */
        fun choices(current: PermissionMode) =
            if (current == BypassPermissions) entries.toList() else entries.filter { it != BypassPermissions }
    }
}

// ---- events ----

enum class ToolKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Mcp, Other;
    companion object {
        fun of(s: String?) = entries.firstOrNull { it.name.equals(s, ignoreCase = true) } ?: Other
    }
}

enum class ToolStatus { Pending, Running, Completed, Failed;
    companion object {
        fun of(s: String?) = entries.firstOrNull { it.name.equals(s, ignoreCase = true) } ?: Pending
    }
}

data class ToolInfo(
    val name: String,
    val kind: ToolKind,
    val title: String,
    val input: JsonElement,
    val locations: List<String> = emptyList(),
) {
    companion object {
        fun parse(o: JsonObject?) = ToolInfo(
            name = o?.str("name") ?: "tool",
            kind = ToolKind.of(o?.str("kind")),
            title = o?.str("title") ?: "",
            input = o?.get("input") ?: JsonNull,
            locations = o?.arr("locations").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull },
        )
    }
}

/** A tool's output as text (diffs and JSON rendered). */
fun toolOutputText(o: JsonObject?): String? = when (o?.str("type")) {
    null -> null
    "text" -> (o.str("text") ?: "") + if (o.bool("truncated") == true) "\n…" else ""
    "diff" -> o.str("diff") ?: ""
    else -> o["value"]?.pretty() ?: ""
}

enum class OptionKind(val wire: String) {
    AllowOnce("allow_once"), AllowAlways("allow_always"), RejectOnce("reject_once"), RejectAlways("reject_always");

    val isAllow get() = this == AllowOnce || this == AllowAlways

    companion object {
        fun of(s: String?) = entries.firstOrNull { it.wire == s } ?: RejectOnce
    }
}

data class PermissionOption(val id: String, val kind: OptionKind, val label: String)

data class PlanEntry(val content: String, val status: String)

sealed interface EventKind {
    data class SessionStarted(val cwd: String?, val start: String) : EventKind
    data class SessionEnded(val reason: String?) : EventKind
    data class SharingChanged(val shared: Boolean) : EventKind
    data object TurnStarted : EventKind
    data class UserMessage(val text: String, val fromPhone: Boolean) : EventKind
    data class AssistantMessage(val id: String, val text: String, val delta: Boolean) : EventKind
    data class Plan(val entries: List<PlanEntry>) : EventKind
    data class ToolCall(val call: String, val tool: ToolInfo, val status: ToolStatus, val output: String?) : EventKind
    data class PermissionRequested(
        val request: String,
        val tool: ToolInfo,
        val reason: String?,
        val options: List<PermissionOption>,
    ) : EventKind
    /** `by`: "phone", "terminal", "other_client" or "daemon"; `outcome`: "selected", "cancelled", "timed_out". */
    data class PermissionResolved(val request: String, val outcome: String, val optionKind: OptionKind?, val by: String) : EventKind
    data object TurnEnded : EventKind
    data class Notice(val text: String, val warning: Boolean) : EventKind
    data class Error(val message: String) : EventKind
    /** A background task (a tool call run with `run_in_background`) finished. */
    data class TaskEnded(val call: String, val status: String, val summary: String?) : EventKind
    data class Unknown(val type: String) : EventKind

    companion object {
        fun parse(o: JsonObject): EventKind = when (val type = o.str("type")) {
            "session_started" -> SessionStarted(o.str("cwd"), o.str("start") ?: "new")
            "session_ended" -> SessionEnded(o.str("reason"))
            "sharing_changed" -> SharingChanged(o.bool("shared") ?: false)
            "turn_started" -> TurnStarted
            "user_message" -> UserMessage(o.str("text") ?: "", o.obj("origin")?.str("from") == "remote")
            "assistant_message" -> AssistantMessage(o.str("message") ?: "", o.str("text") ?: "", o.str("mode") == "delta")
            "plan" -> Plan(o.arr("entries").orEmpty().map {
                val e = it.jsonObject
                PlanEntry(e.str("content") ?: "", e.str("status") ?: "pending")
            })
            "tool_call" -> ToolCall(
                o.str("call") ?: "",
                ToolInfo.parse(o.obj("tool")),
                ToolStatus.of(o.str("status")),
                toolOutputText(o.obj("output")),
            )
            "permission_requested" -> PermissionRequested(
                o.str("request") ?: "",
                ToolInfo.parse(o.obj("tool")),
                o.str("reason"),
                o.arr("options").orEmpty().map {
                    val p = it.jsonObject
                    PermissionOption(p.str("id") ?: "", OptionKind.of(p.str("kind")), p.str("label") ?: "")
                },
            )
            "permission_resolved" -> {
                val out = o.obj("outcome")
                PermissionResolved(
                    o.str("request") ?: "",
                    out?.str("outcome") ?: "cancelled",
                    out?.str("kind")?.let { OptionKind.of(it) },
                    o.obj("by")?.str("by") ?: "daemon",
                )
            }
            "turn_ended" -> TurnEnded
            "notice" -> Notice(o.str("text") ?: "", o.str("level") == "warning")
            "error" -> Error(o.str("message") ?: "")
            "task_ended" -> TaskEnded(o.str("call") ?: "", o.str("status") ?: "completed", o.str("summary"))
            else -> Unknown(type ?: "?")
        }
    }
}

data class Event(val session: String, val seq: Long, val atMs: Long, val kind: EventKind) {
    companion object {
        fun parse(o: JsonObject) = Event(
            session = o.str("session") ?: "",
            seq = o.long("seq") ?: 0,
            atMs = o.long("at_ms") ?: 0,
            kind = o.obj("kind")?.let { EventKind.parse(it) } ?: EventKind.Unknown("?"),
        )
    }
}

// ---- folders (new session) ----

data class FolderEntry(val name: String, val path: String, val isRepo: Boolean)

data class FolderListing(
    val path: String,
    val parent: String?,
    val home: String,
    val entries: List<FolderEntry>,
    val recent: List<String>,
    val agents: List<AgentKind>,
    /** Files directly in `path`, to see what's there. */
    val files: List<String> = emptyList(),
    val moreFiles: Boolean = false,
    val gitBranch: String? = null,
) {
    /** `~/work/styrman` rather than `/Users/me/work/styrman`. */
    fun display(p: String) = when {
        p == home -> "~"
        p.startsWith("$home/") -> "~" + p.removePrefix(home)
        else -> p
    }

    companion object {
        fun parse(o: JsonObject) = FolderListing(
            path = o.str("path") ?: "",
            parent = o.str("parent"),
            home = o.str("home") ?: "",
            entries = o.arr("entries").orEmpty().map {
                val e = it.jsonObject
                FolderEntry(e.str("name") ?: "", e.str("path") ?: "", e.bool("is_repo") ?: false)
            },
            recent = o.arr("recent").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull },
            agents = o.arr("agents").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull }.map { AgentKind.of(it) },
            files = o.arr("files").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull },
            moreFiles = o.bool("more_files") ?: false,
            gitBranch = o.str("git_branch"),
        )
    }
}

data class UsageWindow(val title: String, val usedFraction: Double, val resetsAtMs: Long, val rolling: Boolean)

data class UsageProvider(
    val id: String,
    val estimated: Boolean,
    val observedAtMs: Long,
    val windows: List<UsageWindow>,
) {
    val name: String get() = when (id) {
        "claude" -> "Claude"
        "codex" -> "Codex"
        "gemini" -> "Gemini / Antigravity"
        else -> id
    }

    companion object {
        fun parse(o: JsonObject) = UsageProvider(
            id = o.str("id") ?: "",
            estimated = o.bool("estimated") ?: false,
            observedAtMs = o.long("observed_at_ms") ?: 0,
            windows = o.arr("windows").orEmpty().map { entry ->
                val window = entry.jsonObject
                UsageWindow(
                    title = window.str("title") ?: "",
                    usedFraction = (window["used_fraction"] as? JsonPrimitive)?.doubleOrNull ?: 0.0,
                    resetsAtMs = window.long("resets_at_ms") ?: 0,
                    rolling = window.bool("rolling") ?: false,
                )
            },
        )
    }
}

// ---- server → phone ----

sealed interface ServerMsg {
    data class Hello(val host: String, val warnings: List<String>, val usageSupported: Boolean, val lan: LanRoute? = null) : ServerMsg
    data class Usage(val providers: List<UsageProvider>) : ServerMsg
    data class Sessions(val sessions: List<SessionSummary>) : ServerMsg
    data class SessionUpdated(val session: SessionSummary) : ServerMsg
    data class SessionRemoved(val id: String) : ServerMsg
    data class Events(val session: String, val events: List<Event>) : ServerMsg
    data class Subscribed(val session: String) : ServerMsg
    data class EventMsg(val event: Event) : ServerMsg
    data class PromptQueued(val session: String, val text: String) : ServerMsg
    data class PromptDelivered(val session: String, val text: String) : ServerMsg
    data class Folders(val listing: FolderListing) : ServerMsg
    data class SessionStarting(val session: String) : ServerMsg
    /** Uncommitted changes as a unified diff; `truncated`: cut for length. */
    data class Changes(val session: String, val branch: String?, val diff: String, val truncated: Boolean) : ServerMsg
    /** The terminal as it looks now, while this phone watches it. */
    data class Screen(val session: String, val text: String, val cols: Int) : ServerMsg
    /** The reply being written right now; null: none in progress. */
    data class Preview(val session: String, val text: String?, val status: WorkStatus? = null) : ServerMsg
    data class Error(val message: String) : ServerMsg
    data class Unknown(val type: String) : ServerMsg

    companion object {
        fun parse(text: String): ServerMsg {
            val o = json.parseToJsonElement(text).jsonObject
            return when (val type = o.str("type")) {
                "hello" -> Hello(
                    o.str("host") ?: "",
                    o.arr("warnings").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull },
                    o.bool("usage_supported") ?: false,
                    LanRoute.of(o.arr("lan_urls").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull }, o.str("lan_cert")),
                )
                "usage" -> Usage(o.arr("providers").orEmpty().map { UsageProvider.parse(it.jsonObject) })
                "sessions" -> Sessions(o.arr("sessions").orEmpty().map { SessionSummary.parse(it.jsonObject) })
                "session_updated" -> SessionUpdated(SessionSummary.parse(o.obj("session") ?: JsonObject(emptyMap())))
                "session_removed" -> SessionRemoved(o.str("id") ?: "")
                "events" -> Events(o.str("session") ?: "", o.arr("events").orEmpty().map { Event.parse(it.jsonObject) })
                "subscribed" -> Subscribed(o.str("session") ?: "")
                "event" -> EventMsg(Event.parse(o.obj("event") ?: JsonObject(emptyMap())))
                "prompt_queued" -> PromptQueued(o.str("session") ?: "", o.str("text") ?: "")
                "prompt_delivered" -> PromptDelivered(o.str("session") ?: "", o.str("text") ?: "")
                "folders" -> Folders(FolderListing.parse(o.obj("listing") ?: JsonObject(emptyMap())))
                "session_starting" -> SessionStarting(o.str("session") ?: "")
                "changes" -> Changes(o.str("session") ?: "", o.str("branch"), o.str("diff") ?: "", o.bool("truncated") ?: false)
                "screen" -> Screen(o.str("session") ?: "", o.str("text") ?: "", o.long("cols")?.toInt() ?: 80)
                "preview" -> Preview(o.str("session") ?: "", o.str("text"), o.obj("status")?.let(WorkStatus::of))
                "error" -> Error(o.str("message") ?: "")
                else -> Unknown(type ?: "?")
            }
        }
    }
}

// ---- phone → server ----

object ClientMsg {
    fun getUsage() = buildJsonObject { put("type", "get_usage") }

    fun listSessions() = buildJsonObject { put("type", "list_sessions") }
    fun subscribe(session: String, sinceSeq: Long) =
        buildJsonObject { put("type", "subscribe"); put("session", session); put("since_seq", sinceSeq) }
    fun unsubscribe(session: String) = buildJsonObject { put("type", "unsubscribe"); put("session", session) }
    private fun command(session: String, type: String, fields: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit = {}) =
        buildJsonObject {
            put("type", "command"); put("session", session)
            putJsonObject("command") { put("type", type); fields() }
        }
    fun resolvePermission(session: String, request: String, option: String) =
        command(session, "resolve_permission") { put("request", request); put("option", option) }
    fun sendPrompt(session: String, text: String) = command(session, "send_prompt") { put("text", text) }
    fun stopSharing(session: String) = command(session, "set_sharing") { put("shared", false) }
    fun interrupt(session: String) = command(session, "interrupt")
    fun endSession(session: String) = command(session, "end_session")
    fun setMode(session: String, mode: String) = command(session, "set_mode") { put("mode", mode) }
    fun getChanges(session: String) = buildJsonObject { put("type", "get_changes"); put("session", session) }
    fun watchScreen(session: String) = buildJsonObject { put("type", "watch_screen"); put("session", session) }
    fun listFolders(path: String?) = buildJsonObject {
        put("type", "list_folders"); if (path != null) put("path", path)
    }
    fun createFolder(parent: String, name: String) = buildJsonObject {
        put("type", "create_folder"); put("parent", parent); put("name", name)
    }
    fun startSession(agent: AgentKind, cwd: String, prompt: String?) = buildJsonObject {
        put("type", "start_session"); put("agent", agent.wire); put("cwd", cwd)
        if (!prompt.isNullOrBlank()) put("prompt", prompt)
    }
}

private fun JsonArray?.orEmpty(): List<JsonElement> = this ?: emptyList()

/**
 * How to reach the Mac on the local network when Tailscale is down: `wss://` URLs on
 * private addresses, served with the self-signed certificate whose SHA-256 is [cert].
 */
data class LanRoute(val urls: List<String>, val cert: String) {
    companion object {
        fun of(urls: List<String>, cert: String?): LanRoute? {
            if (cert == null || cert.length != 64 || !cert.all { it in '0'..'9' || it in 'a'..'f' }) return null
            val ok = urls.filter { u ->
                u.startsWith("wss://") && isPrivate(u.removePrefix("wss://").substringBefore('/').substringBeforeLast(':'))
            }
            return if (ok.isEmpty()) null else LanRoute(ok, cert)
        }

        fun isPrivate(host: String): Boolean {
            val p = host.split('.').map { it.toIntOrNull() ?: return false }
            if (p.size != 4 || p.any { it !in 0..255 }) return false
            return p[0] == 10 || (p[0] == 172 && p[1] in 16..31) || (p[0] == 192 && p[1] == 168)
        }
    }
}

/** What the agent's status line says while it works, as its terminal shows it:
 *  `✻ Crunching… (1m 3s · ↓ 2.1k tokens · thinking)`. */
data class WorkStatus(
    val label: String,
    val elapsedSecs: Long? = null,
    val tokens: String? = null,
    val thinking: Boolean = false,
    /** Anything else it says: "running PreToolUse hooks… 2/3", "thought for 3s". */
    val note: String? = null,
) {
    /** The bracketed part, as Claude writes it, with the time `extra` seconds on. */
    fun detail(extra: Long = 0): String = listOfNotNull(
        note,
        elapsedSecs?.let { duration(it + maxOf(0, extra)) },
        tokens,
        "thinking".takeIf { thinking },
    ).joinToString(" · ")

    companion object {
        fun of(o: JsonObject): WorkStatus? = o.str("label")?.let {
            WorkStatus(it, o.long("elapsed_secs"), o.str("tokens"), o.bool("thinking") ?: false, o.str("note"))
        }

        /** "4s", "18m 5s", "1h 2m 3s", like Claude's. */
        fun duration(s: Long): String {
            val (h, m, sec) = Triple(s / 3600, s % 3600 / 60, s % 60)
            return when {
                h > 0 -> "${h}h ${m}m ${sec}s"
                m > 0 -> "${m}m ${sec}s"
                else -> "${sec}s"
            }
        }
    }
}

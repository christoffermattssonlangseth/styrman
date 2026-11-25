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
) {
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
        )
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

data class ToolInfo(val name: String, val kind: ToolKind, val title: String, val input: JsonElement) {
    companion object {
        fun parse(o: JsonObject?) = ToolInfo(
            name = o?.str("name") ?: "tool",
            kind = ToolKind.of(o?.str("kind")),
            title = o?.str("title") ?: "",
            input = o?.get("input") ?: JsonNull,
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

// ---- server → phone ----

sealed interface ServerMsg {
    data class Hello(val host: String, val warnings: List<String>) : ServerMsg
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
    data class Error(val message: String) : ServerMsg
    data class Unknown(val type: String) : ServerMsg

    companion object {
        fun parse(text: String): ServerMsg {
            val o = json.parseToJsonElement(text).jsonObject
            return when (val type = o.str("type")) {
                "hello" -> Hello(o.str("host") ?: "", o.arr("warnings").orEmpty().mapNotNull { (it as? JsonPrimitive)?.contentOrNull })
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
                "error" -> Error(o.str("message") ?: "")
                else -> Unknown(type ?: "?")
            }
        }
    }
}

// ---- phone → server ----

object ClientMsg {
    fun listSessions() = buildJsonObject { put("type", "list_sessions") }
    fun subscribe(session: String, sinceSeq: Long) =
        buildJsonObject { put("type", "subscribe"); put("session", session); put("since_seq", sinceSeq) }
    fun unsubscribe(session: String) = buildJsonObject { put("type", "unsubscribe"); put("session", session) }
    fun resolvePermission(session: String, request: String, option: String) = buildJsonObject {
        put("type", "command"); put("session", session)
        putJsonObject("command") { put("type", "resolve_permission"); put("request", request); put("option", option) }
    }
    fun sendPrompt(session: String, text: String) = buildJsonObject {
        put("type", "command"); put("session", session)
        putJsonObject("command") { put("type", "send_prompt"); put("text", text) }
    }
    fun stopSharing(session: String) = buildJsonObject {
        put("type", "command"); put("session", session)
        putJsonObject("command") { put("type", "set_sharing"); put("shared", false) }
    }
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

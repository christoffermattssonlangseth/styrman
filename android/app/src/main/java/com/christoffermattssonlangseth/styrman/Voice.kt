package com.christoffermattssonlangseth.styrman

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.speech.tts.Voice
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import android.os.Handler
import android.os.Looper
import java.util.Locale

/** Turn a Markdown reply into something pleasant to listen to (same rules as iOS). */
object SpeechText {
    fun clean(markdown: String): String {
        var s = markdown
        s = s.replace(Regex("```[\\s\\S]*?```"), " (code block) ")
        s = s.replace(Regex("\\[([^\\]]+)]\\([^)]+\\)"), "$1")
        s = s.replace(Regex("https?://\\S+"), "a link")
        s = s.replace(Regex("`([^`]*)`"), "$1")
        s = s.replace(Regex("(?m)^\\s{0,3}#{1,6}\\s*"), "")
        s = s.replace(Regex("(\\*\\*|__|\\*|_)(?=\\S)([^*_]+?)(?<=\\S)\\1"), "$2")
        s = s.replace(Regex("(?m)^\\s*([-*+]|\\d+\\.)\\s+"), "")
        s = s.replace(Regex("(?m)^\\s*>\\s?"), "")
        s = s.replace(Regex("(?m)^\\s*\\|?[-:| ]+\\|[-:| ]*$"), "")
        s = s.replace("|", ", ")
        // Paths: just the file name ("src/views/App.kt" → "App.kt").
        s = s.replace(Regex("(?<![\\w.])(?:~|\\.{1,2})?(?:/?[\\w.-]+/)+([\\w.-]+)"), "$1")
        for ((from, to) in listOf("->" to " to ", "=>" to " to ", "→" to " to ", "e.g." to "for example",
            "i.e." to "that is", "&&" to " and ", "✓" to "", "✅" to "", "✗" to "", "❌" to "", "…" to "...")) {
            s = s.replace(from, to)
        }
        s = s.replace(Regex("[ \\t]+"), " ")
        s = s.replace(Regex("\\n{2,}"), "\n")
        return s.trim()
    }

    /** Paragraphs and list items, read one after another with a short pause. */
    fun paragraphs(text: String) = text.split("\n").map { it.trim() }.filter { it.isNotEmpty() }
}

/** Reads replies aloud, each agent in its own voice. */
class Speaker(context: Context) {
    private val prefs = context.getSharedPreferences("voice", Context.MODE_PRIVATE)
    private val main = Handler(Looper.getMainLooper())
    private var ready = false
    private val tts: TextToSpeech = TextToSpeech(context.applicationContext) { status ->
        main.post { ready = status == TextToSpeech.SUCCESS; voices = available() }
    }

    /** The id of the message being read, if any. */
    var speakingId: String? by mutableStateOf(null)
        private set
    var autoRead: Boolean by mutableStateOf(prefs.getBoolean("autoRead", false))
        private set
    var voices: List<Voice> by mutableStateOf(emptyList())
        private set
    private var remaining = 0

    init {
        tts.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
            override fun onStart(id: String?) {}
            override fun onDone(id: String?) {
                if (id?.contains("#pause") != true) main.post { finished() }
            }
            @Deprecated("Deprecated in Java")
            override fun onError(id: String?) {
                if (id?.contains("#pause") != true) main.post { finished() }
            }
            override fun onStop(id: String?, interrupted: Boolean) = main.post { speakingId = null; remaining = 0 }.let {}
        })
    }

    fun updateAutoRead(on: Boolean) {
        autoRead = on
        prefs.edit().putBoolean("autoRead", on).apply()
        if (!on) stop()
    }

    fun toggle(text: String, id: String, agent: AgentKind) = if (speakingId == id) stop() else speak(text, id, agent)

    fun speak(text: String, id: String, agent: AgentKind) {
        val parts = SpeechText.paragraphs(SpeechText.clean(text))
        if (!ready || parts.isEmpty()) return
        stop()
        voiceFor(agent)?.let { tts.voice = it }
        tts.setSpeechRate(0.96f)
        tts.setPitch(1.0f) // pitch-shifting is what makes voices sound synthetic
        speakingId = id
        remaining = parts.size
        parts.forEachIndexed { i, p ->
            tts.speak(p, TextToSpeech.QUEUE_ADD, Bundle(), "$id#$i")
            if (i < parts.size - 1) tts.playSilentUtterance(300, TextToSpeech.QUEUE_ADD, "$id#pause$i")
        }
    }

    fun preview(agent: AgentKind) =
        speak("Hi, this is ${agent.displayName}. I'll read my replies to you like this.", "preview-${agent.wire}", agent)

    fun stop() {
        remaining = 0
        tts.stop()
        speakingId = null
    }

    private fun finished() {
        remaining = (remaining - 1).coerceAtLeast(0)
        if (remaining == 0) speakingId = null
    }

    // ---- choosing voices ----

    val agents = listOf(AgentKind.Claude, AgentKind.Codex, AgentKind.Antigravity)

    /** Installed, offline voices for the phone's language, best quality first. */
    fun available(): List<Voice> {
        if (!ready) return emptyList()
        val lang = Locale.getDefault().language
        var v = tts.voices.orEmpty().filter { !it.isNetworkConnectionRequired && it.locale.language == lang }
        if (v.isEmpty()) v = tts.voices.orEmpty().filter { !it.isNetworkConnectionRequired && it.locale.language == "en" }
        return v.sortedWith(compareByDescending<Voice> { it.quality }.thenBy { it.name })
    }

    fun chosen(agent: AgentKind): String? = prefs.getString("voice.${agent.wire}", null)

    fun choose(name: String?, agent: AgentKind) {
        prefs.edit().apply { if (name == null) remove("voice.${agent.wire}") else putString("voice.${agent.wire}", name) }.apply()
    }

    /** A different one of the best voices per agent (or the chosen one). */
    fun voiceFor(agent: AgentKind): Voice? {
        val all = voices.ifEmpty { available() }
        chosen(agent)?.let { name -> all.firstOrNull { it.name == name }?.let { return it } }
        if (all.isEmpty()) return null
        return all[(agents.indexOf(agent).coerceAtLeast(0)) % all.size]
    }

    fun shutdown() = tts.shutdown()
}

/** Dictation into the message box (Android's speech recognizer). */
class Dictation(context: Context, private val onText: (String) -> Unit) {
    var listening by mutableStateOf(false)
        private set
    private val recognizer: SpeechRecognizer? =
        if (SpeechRecognizer.isRecognitionAvailable(context)) SpeechRecognizer.createSpeechRecognizer(context) else null

    val available get() = recognizer != null

    init {
        recognizer?.setRecognitionListener(object : RecognitionListener {
            override fun onPartialResults(b: Bundle?) = deliver(b)
            override fun onResults(b: Bundle?) { deliver(b); listening = false }
            override fun onError(error: Int) { listening = false }
            override fun onEndOfSpeech() {}
            override fun onReadyForSpeech(p: Bundle?) {}
            override fun onBeginningOfSpeech() {}
            override fun onRmsChanged(rms: Float) {}
            override fun onBufferReceived(b: ByteArray?) {}
            override fun onEvent(t: Int, p: Bundle?) {}
        })
    }

    private fun deliver(b: Bundle?) {
        b?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.let(onText)
    }

    fun toggle() = if (listening) stop() else start()

    fun start() {
        val r = recognizer ?: return
        val i = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true) // keep prompts on the phone when possible
        }
        listening = true
        r.startListening(i)
    }

    fun stop() {
        recognizer?.stopListening()
        listening = false
    }

    fun destroy() = recognizer?.destroy()
}

import AVFoundation
import Observation
import Speech

// Voice, both ways, entirely on the phone:
// - Dictation: speech → text in the message box (Apple's speech recognizer, kept on
//   the device when the device supports it, so prompts about your code stay local).
// - Read aloud: replies → speech (AVSpeechSynthesizer), with a different voice per agent.

/// Dictation into the message box.
@MainActor @Observable
final class Dictation {
    enum State: Equatable { case idle, starting, listening, denied, unavailable }

    private(set) var state: State = .idle
    /// Live transcript of the current dictation.
    private(set) var transcript = ""

    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var recognizer: SFSpeechRecognizer?

    var isListening: Bool { state == .listening || state == .starting }

    func toggle() async {
        if isListening { stop() } else { await start() }
    }

    func start() async {
        guard !isListening else { return }
        state = .starting
        transcript = ""
        guard await Self.authorized() else {
            state = .denied
            return
        }
        guard let recognizer = SFSpeechRecognizer(locale: .current) ?? SFSpeechRecognizer(locale: Locale(identifier: "en-US")),
              recognizer.isAvailable
        else {
            state = .unavailable
            return
        }
        self.recognizer = recognizer
        Speaker.shared.stop()  // don't transcribe our own read-aloud
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playAndRecord, mode: .measurement, options: [.duckOthers, .defaultToSpeaker, .allowBluetoothHFP])
            try session.setActive(true, options: .notifyOthersOnDeactivation)

            let request = SFSpeechAudioBufferRecognitionRequest()
            request.shouldReportPartialResults = true
            request.addsPunctuation = true
            if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
            self.request = request

            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            input.removeTap(onBus: 0)
            // The tap runs on an audio thread: it only hands buffers to the request.
            input.installTap(onBus: 0, bufferSize: 1024, format: format, block: Self.tap(feeding: request))
            engine.prepare()
            try engine.start()

            task = recognizer.recognitionTask(
                with: request,
                resultHandler: Self.results { [weak self] text, ended in
                    Task { @MainActor in
                        guard let self else { return }
                        if let text { self.transcript = text }
                        if ended { self.finish() }
                    }
                })
            state = .listening
        } catch {
            finish()
            state = .unavailable
        }
    }

    /// Stop listening; the transcript so far stays.
    func stop() {
        guard isListening else { return }
        request?.endAudio()
        finish()
    }

    private func finish() {
        if engine.isRunning { engine.stop() }
        engine.inputNode.removeTap(onBus: 0)
        task?.cancel()
        task = nil
        request = nil
        if state == .listening || state == .starting { state = .idle }
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    // The callbacks below are called by iOS on its own threads. They're made here,
    // outside the main actor: a closure written inside a @MainActor method is
    // assumed to run on the main thread, and Swift stops the app when it doesn't.

    nonisolated static func tap(feeding request: SFSpeechAudioBufferRecognitionRequest) -> AVAudioNodeTapBlock {
        nonisolated(unsafe) let request = request
        return { buffer, _ in request.append(buffer) }
    }

    nonisolated static func results(
        _ handle: @escaping @Sendable (_ text: String?, _ ended: Bool) -> Void
    ) -> @Sendable (SFSpeechRecognitionResult?, (any Error)?) -> Void {
        { result, error in
            handle(result?.bestTranscription.formattedString, (result?.isFinal ?? false) || error != nil)
        }
    }

    /// How the speech permission is asked for (tests replace it to answer from another thread).
    typealias AuthRequester = @Sendable (@escaping @Sendable (SFSpeechRecognizerAuthorizationStatus) -> Void) -> Void

    nonisolated static let systemAuth: AuthRequester = { reply in
        SFSpeechRecognizer.requestAuthorization(reply)
    }

    /// The handler iOS calls with the answer, on one of its own threads. Built in a
    /// synchronous nonisolated function so it carries no actor isolation at all.
    nonisolated static func authReply(_ done: @escaping @Sendable (Bool) -> Void)
        -> @Sendable (SFSpeechRecognizerAuthorizationStatus) -> Void
    {
        { status in done(status == .authorized) }
    }

    nonisolated static func speechAuthorized(using request: AuthRequester = systemAuth) async -> Bool {
        await withCheckedContinuation { (c: CheckedContinuation<Bool, Never>) in
            request(authReply { c.resume(returning: $0) })
        }
    }

    nonisolated static func authorized() async -> Bool {
        guard await speechAuthorized() else { return false }
        return await AVAudioApplication.requestRecordPermission()
    }
}

/// Reads replies aloud, each agent in its own voice.
@MainActor @Observable
final class Speaker: NSObject, AVSpeechSynthesizerDelegate {
    static let shared = Speaker()

    /// The id of the message being read, if any.
    private(set) var speakingId: String?

    /// Read new replies aloud while a session is open.
    var autoRead: Bool = UserDefaults.standard.bool(forKey: "voice.autoRead") {
        didSet { UserDefaults.standard.set(autoRead, forKey: "voice.autoRead") }
    }

    private let synth = AVSpeechSynthesizer()

    override init() {
        super.init()
        synth.delegate = self
    }

    func toggle(_ text: String, id: String, agent: AgentKind) {
        if speakingId == id { stop() } else { speak(text, id: id, agent: agent) }
    }

    func speak(_ text: String, id: String, agent: AgentKind) {
        let spoken = SpeechText.clean(text)
        guard !spoken.isEmpty else { return }
        stop()
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        try? AVAudioSession.sharedInstance().setActive(true)
        let u = AVSpeechUtterance(string: spoken)
        let style = VoiceChoice.style(for: agent)
        u.voice = VoiceChoice.voice(for: agent)
        u.pitchMultiplier = style.pitch
        u.rate = style.rate
        speakingId = id
        synth.speak(u)
    }

    /// Hear what an agent sounds like (settings).
    func preview(_ agent: AgentKind) {
        speak("Hi, this is \(agent.displayName). I'll read my replies to you like this.", id: "preview-\(agent.rawValue)", agent: agent)
    }

    func stop() {
        if synth.isSpeaking { synth.stopSpeaking(at: .immediate) }
        speakingId = nil
    }

    nonisolated func speechSynthesizer(_ s: AVSpeechSynthesizer, didFinish u: AVSpeechUtterance) {
        Task { @MainActor in self.done() }
    }

    nonisolated func speechSynthesizer(_ s: AVSpeechSynthesizer, didCancel u: AVSpeechUtterance) {
        Task { @MainActor in self.done() }
    }

    private func done() {
        guard !synth.isSpeaking else { return }
        speakingId = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}

/// Which voice each agent speaks with. By default each agent gets a different one of
/// the best voices installed for the phone's language, plus a slightly different
/// pitch, so they're distinct even with a single voice. Settings can override it.
enum VoiceChoice {
    static let agents: [AgentKind] = [.claude, .codex, .antigravity]

    struct Style { let pitch: Float; let rate: Float }

    static func style(for agent: AgentKind) -> Style {
        let base = AVSpeechUtteranceDefaultSpeechRate
        switch agent {
        case .claude: return Style(pitch: 1.0, rate: base)
        case .codex: return Style(pitch: 0.9, rate: base * 1.02)
        case .antigravity: return Style(pitch: 1.12, rate: base)
        default: return Style(pitch: 1.0, rate: base)
        }
    }

    /// Voices for the phone's language, best quality first.
    static func available() -> [AVSpeechSynthesisVoice] {
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        var voices = AVSpeechSynthesisVoice.speechVoices().filter { $0.language.hasPrefix(lang) }
        if voices.isEmpty { voices = AVSpeechSynthesisVoice.speechVoices().filter { $0.language.hasPrefix("en") } }
        // Novelty voices (Bells, Bubbles, …) are not for reading code reviews.
        voices = voices.filter { !$0.voiceTraits.contains(.isNoveltyVoice) }
        return voices.sorted {
            ($0.quality.rawValue, $0.language == Locale.current.identifier(.bcp47) ? 1 : 0, $0.name)
                > ($1.quality.rawValue, $1.language == Locale.current.identifier(.bcp47) ? 1 : 0, $1.name)
        }
    }

    static func key(_ agent: AgentKind) -> String { "voice.\(agent.rawValue)" }

    /// The chosen voice identifier for an agent (nil: automatic).
    static func chosen(_ agent: AgentKind) -> String? {
        UserDefaults.standard.string(forKey: key(agent))
    }

    static func choose(_ identifier: String?, for agent: AgentKind) {
        UserDefaults.standard.set(identifier, forKey: key(agent))
    }

    static func voice(for agent: AgentKind) -> AVSpeechSynthesisVoice? {
        if let id = chosen(agent), let v = AVSpeechSynthesisVoice(identifier: id) { return v }
        let voices = available()
        guard !voices.isEmpty else { return AVSpeechSynthesisVoice(language: Locale.current.identifier(.bcp47)) }
        // Prefer a different voice per agent; alternate genders when both exist.
        let ordered = interleavedByGender(voices)
        let index = agents.firstIndex(of: agent) ?? 0
        return ordered[index % ordered.count]
    }

    private static func interleavedByGender(_ voices: [AVSpeechSynthesisVoice]) -> [AVSpeechSynthesisVoice] {
        let female = voices.filter { $0.gender == .female }
        let male = voices.filter { $0.gender == .male }
        let other = voices.filter { $0.gender == .unspecified }
        var out: [AVSpeechSynthesisVoice] = []
        // Claude: female, Codex: male, agy: female (a different one), then the rest.
        var f = female.makeIterator(), m = male.makeIterator()
        for slot in 0..<max(female.count, male.count) * 2 {
            if let v = slot % 2 == 0 ? f.next() : m.next() { out.append(v) }
        }
        out.append(contentsOf: other)
        return out.isEmpty ? voices : out
    }
}

/// Turn a Markdown reply into something pleasant to listen to.
enum SpeechText {
    static func clean(_ markdown: String) -> String {
        var s = markdown
        // Fenced code blocks: say that there is code rather than reading it.
        s = s.replacingOccurrences(of: "```[\\s\\S]*?```", with: " (code block) ", options: .regularExpression)
        // Links: keep the label.
        s = s.replacingOccurrences(of: "\\[([^\\]]+)\\]\\([^)]+\\)", with: "$1", options: .regularExpression)
        // Bare URLs.
        s = s.replacingOccurrences(of: "https?://\\S+", with: "a link", options: .regularExpression)
        // Inline code keeps its text.
        s = s.replacingOccurrences(of: "`([^`]*)`", with: "$1", options: .regularExpression)
        // Headings, emphasis, list bullets, table pipes, quotes.
        s = s.replacingOccurrences(of: "(?m)^\\s{0,3}#{1,6}\\s*", with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: "(\\*\\*|__|\\*|_)(?=\\S)([^*_]+?)(?<=\\S)\\1", with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: "(?m)^\\s*([-*+]|\\d+\\.)\\s+", with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: "(?m)^\\s*>\\s?", with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: "(?m)^\\s*\\|?[-:| ]+\\|[-:| ]*$", with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: "|", with: ", ")
        s = s.replacingOccurrences(of: "[ \\t]+", with: " ", options: .regularExpression)
        s = s.replacingOccurrences(of: "\\n{2,}", with: "\n", options: .regularExpression)
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

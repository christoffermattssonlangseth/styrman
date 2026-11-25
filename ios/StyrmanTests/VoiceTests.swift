import AVFoundation
import Speech
import Testing
@testable import Styrman

@Suite struct VoiceTests {
    @Test func repliesAreCleanedUpForListening() {
        let reply = """
        ## Done

        I fixed **the bug** in `parse_args` — see [the docs](https://example.com/x).

        ```rust
        fn main() {}
        ```

        - first
        - second
        """
        let spoken = SpeechText.clean(reply)
        #expect(spoken.contains("Done"))
        #expect(spoken.contains("I fixed the bug in parse_args"))
        #expect(spoken.contains("see the docs"))
        #expect(spoken.contains("(code block)"))
        #expect(!spoken.contains("fn main"))
        #expect(!spoken.contains("#"))
        #expect(!spoken.contains("**"))
        #expect(!spoken.contains("`"))
        #expect(!spoken.contains("https://"))
        #expect(spoken.contains("first\nsecond"))
    }

    @Test func bareLinksAreNotReadOut() {
        #expect(SpeechText.clean("Open https://github.com/a/b/pull/12 now") == "Open a link now")
    }

    @Test @MainActor func agentsGetDifferentVoicesWhenTheDeviceHasThem() {
        let ids = VoiceChoice.agents.compactMap { VoiceChoice.voice(for: $0)?.identifier }
        let available = VoiceChoice.available().count
        if available >= 3 {
            #expect(Set(ids).count == 3, "three distinct voices from \(available)")
        }
        // Even with one voice, pitch keeps them apart.
        let pitches = VoiceChoice.agents.map { VoiceChoice.style(for: $0).pitch }
        #expect(Set(pitches).count == 3)
    }

    @Test func aChosenVoiceWins() throws {
        let voices = VoiceChoice.available()
        try #require(!voices.isEmpty)
        let pick = voices.last!.identifier
        VoiceChoice.choose(pick, for: .codex)
        defer { VoiceChoice.choose(nil, for: .codex) }
        #expect(VoiceChoice.voice(for: .codex)?.identifier == pick)
    }
}

/// iOS calls the speech callbacks on its own threads; they must not assume the main actor
/// (that crashed the app when tapping the mic).
@Suite struct DictationThreadingTests {
    @Test func callbacksRunOffTheMainThread() async throws {
        let request = SFSpeechAudioBufferRecognitionRequest()
        let tap = Dictation.tap(feeding: request)
        let handler = Dictation.results { _, _ in }
        let format = try #require(AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1))
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 160))
        buffer.frameLength = 160
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            DispatchQueue.global().async {
                tap(buffer, AVAudioTime(sampleTime: 0, atRate: 16_000))
                handler(nil, nil)
                done.resume()
            }
        }
    }
}

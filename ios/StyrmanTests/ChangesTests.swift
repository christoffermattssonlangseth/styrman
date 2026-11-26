import Foundation
import Testing
@testable import Styrman

@Suite struct ChangesTests {
    // As styrd sends it: `git diff HEAD` plus an untracked file as an addition.
    let diff = """
    diff --git a/src/upload.rs b/src/upload.rs
    index 3b18e51..a9f4c2d 100644
    --- a/src/upload.rs
    +++ b/src/upload.rs
    @@ -40,4 +40,5 @@ impl Client {
         pub async fn upload(&self, req: Request) -> Result<Response> {
    -        let res = self.send(req).await?;
    +        let res = self.retry(req).await?;
    +        log::debug!("sent");
             res.error_for_status()
    diff --git a/notes.txt b/notes.txt
    new file mode 100644
    --- /dev/null
    +++ b/notes.txt
    @@ -0,0 +1,2 @@
    +hello
    +world

    """

    @Test func filesAndLineNumbers() {
        let files = GitChanges.files(diff)
        #expect(files.map(\.path) == ["src/upload.rs", "notes.txt"])
        let up = files[0]
        #expect(up.added == 2 && up.removed == 1)
        #expect(up.lines[0].kind == .context && up.lines[0].old == 40 && up.lines[0].new == 40)
        #expect(up.lines[1].kind == .removed && up.lines[1].old == 41 && up.lines[1].new == nil)
        #expect(up.lines[2].kind == .added && up.lines[2].new == 41)
        #expect(up.lines[3].new == 42)
        #expect(up.lines[4].kind == .context && up.lines[4].old == 42 && up.lines[4].new == 43)
        #expect(files[1].isNew && files[1].lines.map(\.new) == [1, 2])
    }

    @Test func commentsBecomeOneMessage() {
        let files = GitChanges.files(diff)
        let msg = ReviewMessage.compose([
            ReviewComment(path: "src/upload.rs", line: files[0].lines[2], text: "Cap the retries at 3."),
            ReviewComment(path: "src/upload.rs", line: files[0].lines[1], text: "Why did this go?"),
        ])
        #expect(msg.contains("1. `src/upload.rs` line 41: `let res = self.retry(req).await?;`\n   Cap the retries at 3."))
        #expect(msg.contains("2. `src/upload.rs` (removed line 41): `let res = self.send(req).await?;`"))
    }
}

@Suite struct ReplyMarkdownTests {
    @Test func markdownTablesBecomeRows() {
        let reply = """
        What's in it

        | Folder | Contents |
        |---|---|
        | `runs/` | 638 run folders |
        | **docs/** | Paper drafts |

        The main data isn't in the repo.
        """
        #expect(ReplyMarkdown.readable(reply) == """
        What's in it

        **`runs/`** — 638 run folders
        **docs/** — Paper drafts

        The main data isn't in the repo.
        """)
    }

    @Test func widerTablesNameTheirColumns() {
        let t = "| Model | Speed | Cost |\n|:--|--:|---|\n| Haiku | fast | low |"
        #expect(ReplyMarkdown.readable(t) == "**Haiku** — Speed: fast · Cost: low")
    }

    @Test func previewRowsWithoutDividerAndPlainTextUntouched() {
        // As the live preview reads a terminal's box table.
        #expect(ReplyMarkdown.readable("| Folder | Contents |\n| runs/ | 638 |") == "**runs/** — 638")
        let plain = "Use a | b for a pipe.\n- item"
        #expect(ReplyMarkdown.readable(plain) == plain)
    }
}

@Suite struct ReplyBlockTests {
    @Test func tablesSplitOutOfText() {
        let reply = "Intro\n\n| A | B |\n|---|---|\n| 1 | 2 |\n| 3 |\n\nAfter"
        #expect(ReplyMarkdown.blocks(reply) == [
            .text("Intro"),
            .table(header: ["A", "B"], rows: [["1", "2"], ["3", ""]]),
            .text("After"),
        ])
        // Only dividers: not a table, and no crash.
        #expect(ReplyMarkdown.blocks("|---|\n|---|") == [.text("|---|\n|---|")])
    }
}

@Suite struct CommandTitleTests {
    @Test func cdIntoTheSessionFolderIsDropped() {
        let cwd = "/Users/me/work/reCORD"
        #expect(withoutCd("cd /Users/me/work/reCORD && cat > a.py <<'EOF'", cwd: cwd) == "cat > a.py <<'EOF'")
        #expect(withoutCd("cd \"/Users/me/work/reCORD\"; ls", cwd: cwd) == "ls")
        // Somewhere else: kept (it matters when approving).
        #expect(withoutCd("cd /etc && rm -rf .", cwd: cwd) == "cd /etc && rm -rf .")
        #expect(withoutCd("cd /Users/me/work/reCORD-old && ls", cwd: cwd) == "cd /Users/me/work/reCORD-old && ls")
        #expect(withoutCd("ls", cwd: cwd) == "ls")
    }
}

@Suite struct UsageTextTests {
    @Test func shortDurations() {
        #expect(UsageText.short(45) == "45 s")
        #expect(UsageText.short(58 * 60 + 39) == "58 min")
        #expect(UsageText.short(3_600 + 58 * 60) == "1 h 58 min")
        #expect(UsageText.short(5 * 86_400 + 9 * 3_600 + 120) == "5 d 9 h")
    }
}

struct FlowingPreviewTests {
    @Test func theLastFewWordsAreTheFreshOnes() {
        let (settled, fresh) = LivePreviewRow.split("Yes, everything is fine. The one thing left")
        #expect(settled == "Yes, everything is fine. ")
        #expect(fresh == "The one thing left")
        #expect(LivePreviewRow.split("Hi") == ("", "Hi"))
        #expect(LivePreviewRow.split("") == ("", ""))
    }
}

struct FormattedPreviewTests {
    @Test func markersNeverShowWhileRevealing() {
        let full = LivePreviewRow.formatted("Run `git status` for a **clean baseline** first.")
        #expect(String(full.characters) == "Run git status for a clean baseline first.")
        // Half-way through the bold words: no stray `**`.
        let half = LivePreviewRow.reveal(full, upTo: 28)
        #expect(String(half.characters) == "Run git status for a clean b")
        #expect(LivePreviewRow.plain("a **b") == "a **b") // unclosed: as it is
    }
}

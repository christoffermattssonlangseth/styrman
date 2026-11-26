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

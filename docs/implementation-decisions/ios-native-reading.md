# Native iOS result and activity reading

The iOS18 client uses Textual0.5.0 behind `RichTextContent`; the package graph
is locked in the Xcode project's `Package.resolved`. It renders Foundation
Markdown presentation intents, not HTML or a WebView. Operator input remains
verbatim with its original newlines. Brief and assistant text share the native
reader; raw records remain an explicit secondary disclosure.

The parser removes every image URL attribute and makes the alt text an explicit
link. It does not enable emoji/math extensions or automatic resource loading.
Only HTTP/HTTPS/mailto URLs go to the system. Workspace references and local-host
file URIs go to Holon's authorized resolver, never the iPhone filesystem.
Absolute paths are linked only in inline code; fenced source is not linked.
Literal paths preserve percent signs, fragments and question marks. Relative
references require a complete server-issued base file location, not the active
Agent's guessed working directory. Unsupported links fail visibly.

Textual's packaged Prism tokenizer runs its fixed local grammar through
JavaScriptCore. User code is an argument to tokenization, not evaluated source.
Third-party licenses ship in `ThirdPartyNotices.txt`; the transitive font bundle
retains its own font notices. No session material is passed to a renderer.

Activity reads remain supplemental to the checkpoint-based conversation model.
They validate runtime, epoch, visibility, version and turn before display.
Pages merge by stable ID/revision/key, keep at most180 records in a moving older
window, and retain the server's next-before cursor. Continued paging can reach
all retained records without retaining an unbounded array; a separate action
returns to the newest page. Page cursors never become live SSE resume cursors.
Changed/rejected pages retain the current window and offer an explicit fresh
read, rather than silently joining revisions or repeatedly replaying a cursor.

Only canonical activity membership authorizes tool/transcript detail requests.
Responses are Agent/turn checked, bounded to1MiB, and never reused after a detail
invalidation, stream gap, selection or identity change. Brief reads and activity
requests share a two-request bound; visible brief loads wait in a bounded queue.

The native scroll position starts at latest unless restoring a confirmed turn.
Near the bottom, new content follows the bottom edge; reviewing earlier content
keeps its position and offers a latest/new-content action. Cumulative read state
still requires explicit confirmation and the existing visible-brief/identity
checks. There is no automatic read acknowledgement or background stream.

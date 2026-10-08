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
Absolute Markdown links, inline paths and boundary-delimited bare paths share
Android/Web classification; fenced source and existing links are not re-scanned.
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
The explicit20-turn presentation window uses a regular vertical stack with
actual content heights. Final historical-scroll acceptance reproduced a native
layout loop with lazy offscreen estimates; history remains bounded and paged,
and brief fetching still requires viewport visibility.
Opening older/newer windows explicitly suspends tail following, including when
the newer window reaches the latest turn. Brief hydration must not override
that top-of-window request using the previous viewport's near-bottom value.
Following resumes only through Latest or a user scroll ending at the live tail.
ID/edge navigation uses native bound ScrollPosition, not a one-shot proxy call
before the replacement window's layout has committed.

Long source files use explicit16KiB disk pages, not a lazy stack with estimated
offscreen text heights. Previous/next, literal search and End remain available;
End selects the actual last page before positioning its loaded text. Search
context may append a bounded next-page prefix so split literals remain readable.
The complete download/index keeps its16MiB budget and identity/root checks.
Native link interaction stays active in rich content. Simulator acceptance on
iOS26.5 confirmed that Textual's UIKit selection overlay swallowed link taps;
removing it restored actual relative-file navigation. Rich content therefore
offers a context-menu/accessibility **Select source text** action opening a
read-only native UITextView with the verbatim source and no link detectors.
Flattening Foundation Markdown characters would concatenate blocks, so source
selection deliberately retains Markdown and original separators. It is revoked when the source changes or
disappears. Plain source keeps local SwiftUI selection. This adapter tradeoff
does not alter file authorization or enable external resources.

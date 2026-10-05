# File preview and desktop actions

File preview prioritizes content: one filename, a bounded location row, one
reading toolbar, and file details on demand. Narrow panels switch between the
folder and preview; wide panels can keep both. File/root identity and resolver
authorization are preserved even when long names are visually truncated.

The desktop API is intentionally separate from the connection `local` mode,
which means same-origin HTTP rather than the user's machine. Finder support is
an explicit startup opt-in restricted to macOS, independent of LAN or Serve
exposure. An authenticated caller can reveal a validated workspace file in the
**daemon host's** Finder, including through a remote API. Authentication,
workspace/path authorization, and the desktop endpoint's cross-site checks
remain enforced; no same-device proof is required.

The Web UI shows this capability only when the browser uses a localhost or
loopback entry point. This is a misclick-prevention policy, not an authorization
boundary: a browser on the same machine using a LAN IP or Serve domain also
hides the action, while an authorized remote API can still invoke it.
Other operating systems and editor integrations can add distinct capabilities
later; no generic command-execution endpoint is introduced.

# File preview and desktop actions

File preview prioritizes content: one filename, a bounded location row, one
reading toolbar, and file details on demand. Narrow panels switch between the
folder and preview; wide panels can keep both. File/root identity and resolver
authorization are preserved even when long names are visually truncated.

The desktop API is intentionally separate from the connection `local` mode,
which means same-origin HTTP rather than the user's machine. Finder support is
an explicit startup opt-in restricted to macOS and loopback listeners. It is
not autodetection and should not be enabled for tunneled/proxied instances.
Other operating systems and editor integrations can add distinct capabilities
later; no generic command-execution endpoint is introduced.

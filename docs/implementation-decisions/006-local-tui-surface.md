# Local TUI Surface

Decision:

- add `holon tui` as a thin operator console on top of `holon serve`
- keep `serve` as the sole owner of `RuntimeHost`
- keep the TUI chat-first with overlays instead of pane-focus navigation

Reason:

- runtime lifecycle should not be tied to one terminal session
- operators need continuous visibility without manual command stitching
- a thin UI avoids inventing a TUI-first architecture

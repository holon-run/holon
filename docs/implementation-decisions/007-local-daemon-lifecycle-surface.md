# Local Daemon Lifecycle Surface

Decision:

- keep `holon serve` as the foreground runtime owner
- add `holon daemon start|stop|status|restart|logs` as a lifecycle layer
- persist runtime metadata under `<holon_home>/run/`

Reason:

- operators need a first-class local lifecycle surface
- startup recovery and socket ownership need explicit handling
- daemon inspection should not require opening logs first for every issue

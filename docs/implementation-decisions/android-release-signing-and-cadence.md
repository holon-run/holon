# Android release signing and cadence

Publish Android APK/AAB assets alongside the existing Holon release, using the
root Cargo version and a deterministic monotonic Android versionCode. This
keeps downloads, checksums, and build identity in one release without adding
another tag namespace or claiming strict app/daemon version lockstep.

Use a dedicated long-lived app signing identity, separate from developer debug
keys. Private signing material is held outside the repository and in Actions
secrets; a checked-in public fingerprint makes accidental identity replacement
fail closed. Only trusted release/main jobs sign, and public release publication
waits for verified Android artifacts as well as existing runtime gates.

Manual CI builds produce temporary artifacts only. Independent Android cadence,
store upload keys, Play enrollment and debug-install migration remain explicit
future decisions. See [Android release operations](../../apps/android/RELEASING.md)
and [#3229](https://github.com/holon-run/holon/issues/3229).

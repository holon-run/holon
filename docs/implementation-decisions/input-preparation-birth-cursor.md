# Initialize an empty Input birth cursor before reloading history

An owner-held preparation can miss the Input query birth cursor after Scheduling
and its selected history are already qualified. The existing fallback repeated
the full Input, Scheduling, Episode, queue, and Current loads under the global
SQLite writer to create that cursor.

For requests that can use an empty birth frontier, recheck the scoped identity
and published State in a short writer transaction and initialize the cursor
idempotently. Then load the complete view in one pooled read transaction and
revalidate the original owner publication before installation. The cursor
contains no historical membership. A subsequent qualification miss still
requires the existing scoped qualification path, and a failed read leaves the
published root intact.

Set `HOLON_TRACE_INPUT_PREPARATION_FIXTURE=1` and run the
`input_birth_cursor_preparation_preserves_qualified_history` library test with
`HOLON_LIB_TEST_SHARD=a` and `--nocapture` to compare the 16 and 256 entry
fixtures. The read trace identifies the qualification miss and elapsed time.
The writer trace reports held lock time for the cursor and full qualification
transactions separately.

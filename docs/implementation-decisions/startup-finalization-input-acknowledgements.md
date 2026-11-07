# Startup finalization acknowledges its execution source

The existing startup finalizer changes an interrupted Attempt to settled when
its exact attached Turn has already reached another terminal state. It must
participate in the same original transaction's Input, Current Queue and Event
history acknowledgements, just like startup interruption.

The startup footprint therefore includes already interrupted Attempts, subject
to the existing membership checks. The finalizer returns canonical SQL
readbacks and its actual command to the original recovery actor. If one
startup first interrupts and then finalizes an Attempt, only the last readback
for that evidence key is published. Queue readbacks and Event ACKs join the
same transaction. An abort rolls back both canonical changes and projections.

Unqualified history receives no new Input membership. The consistency checks
still reject changed canonical rows. This forward writer fix does not adopt or
repair projections left stale by an earlier installed binary; those require
separate bounded recovery using the original fixed qualification and exact
canonical command evidence.

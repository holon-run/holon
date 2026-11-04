# Queue claim future stack boundary

Keep `RuntimeMutation::commit_prepared_queue` behind an out-of-line boxed future.
The implementation remains in `commit_prepared_queue_inner`; callers still await
the same operation under the original mutation reservation.

A normal-stack task-rejoin regression overflowed during Context State decoding
inside queue-claim preflight. Its captured stack showed roughly 728 KB retained
by the enclosing scheduler preparation poll frame. Boxing only that outer
preparation future did not separate the queue commit future embedded inside it.
The queue commit boundary keeps that nested future out of its caller's frame
without increasing thread-stack limits.

This is storage indirection, not callback or future type erasure. SQL, witness
completion, publication, cancellation points, and drop order remain unchanged.
The original task-rejoin test and queue-claim tests remain the verification
targets; a successful cached diagnostic does not replace cold-build acceptance.

# Waiting for the original TaskStop result

TaskStop's initial ACK can mark a non-command Task cancelled before its
independently owned Result operation publishes feedback. The original Task ACK
keeps its frozen body, including an absent parent Message pointer.

Resolve terminal waits through the qualified exact-generation Stop head when
that pointer is absent. Pending original Result responsibility permits normal
wait registration. Completed original Result responsibility supplies its frozen
Message identity, followed by the existing canonical Message and settlement
checks. Require the current cancelled status to match the original ACK.

Keep ordinary missing evidence fail-closed. A Task status, guessed Message ID,
foreign generation or arbitrary historical Stop cannot acknowledge a result.
Wait registration and Result admission keep the existing owner reservation and
Task expectation, so publication racing registration still uses the normal
transaction and wake protocol.

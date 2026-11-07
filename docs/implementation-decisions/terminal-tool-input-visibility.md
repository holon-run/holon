# Terminal tools must observe newly admitted operator input

Live node acceptance admitted operator input immediately before an older
WaitFor tool proposal. The terminal tool then ended the Turn without another
model round, acknowledging the input without letting the model read it.

When a round admits new operator input before tool execution, return a
recoverable operator_input_changed result for WaitFor, CompleteWorkItem and
PickWorkItem. Keep the original admitted input in the next-round conversation;
that round decides again. Ordinary tools keep their existing execution policy.
Late input after an actual terminal transition remains queued for the next
activation. Do not replay the operator request or commit an obsolete terminal
decision and then try to undo it.

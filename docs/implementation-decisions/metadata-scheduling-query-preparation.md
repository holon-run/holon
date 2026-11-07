# Scheduling queries after metadata acknowledgements

Metadata acknowledgements deliberately invalidate Current and Scheduling
publications. Related deletion scopes can include an active parent whose State,
Input, Work and Task views remain present. Preparing Current alone leaves
scheduler-facing status and Work result queries unable to capture Scheduling;
a Work write may already be committed when its result projection fails.

At accepted full/compact status, roster and Work list/result query boundaries,
explicitly reconstruct missing Scheduling from its existing complete current
qualification in the same transaction as Current. Bind identity and canonical
State to the held owner publication. The scoped roster uses its original
read-only pool and cancellation budget; reconstruction writes no qualification
sidecars and never adopts changed or unqualified canonical bodies.

Keep metadata invalidation and pure capture behavior unchanged. Ready reads
reuse their existing publication. A missing or changed current proof still
fails closed; querying cannot turn status or an arbitrary row into an ACK.

The registered blocking `/state` worker and nested child summaries use an
explicit scoped acquisition. Preparation and the final parent capture share one
owner reservation, so a second metadata ACK cannot invalidate the cut between
them. Release that parent owner before taking any child owner. Pure scoped
capture components remain separate and continue to reject preparation debt.

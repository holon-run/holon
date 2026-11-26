# Custom Provider Creation by Transport Declaration

Decision:

- `providers.<id>.transport` is the only key that creates a custom provider, and
  only when `<id>` is neither configured in `config.json` nor a built-in
  provider id. Every other provider key keeps reporting `provider <id> not
  found` for unknown ids.
- The config-update handler pre-scans the batch for those declarations and
  inserts a stub (`transport` from the declared value, empty `base_url`,
  `auth` at its `none/none` default) before applying the updates in request
  order, so `base_url` before `transport` in one batch works.
- A created provider must carry a valid `providers.<id>.base_url` in the same
  batch. Otherwise the batch is rejected with a reason that names both keys.
- Creation and field changes classify as `accepted_reload_scheduled`;
  `accepted_requires_restart` stays reserved for `runtime.db_readers.*`.

Reason:

- the Web GUI could not create a provider because the persistence layer rejected
  unknown ids, so the missing piece was a creation contract rather than a
  rendering gap;
- adding a dedicated create endpoint would duplicate provider persistence,
  validation, reload scheduling and credential rules into a second write path
  with a parallel effect classification to keep in sync, while whole-provider
  removal already lives on the same key surface (`unset providers.<id>`);
- requiring only an explicit transport keeps typo diagnostics intact: a
  misspelled id on any other key still fails instead of silently creating an
  empty provider.

Preserved boundary:

- the response `runtime_surface` is still a pre-reload snapshot. A client must
  treat `accepted_reload_scheduled` plus a converged reload generation as the
  signal to refresh, not the same response's provider list;
- `ProviderId::parse` lowercases ids, so uppercase input is accepted as its
  lowercase form. The Web form's `^[a-z0-9_-]+$` rule is a UI convention; the
  config layer stays authoritative.

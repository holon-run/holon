# 077 Xiaomi Model Catalog

## Decision

The built-in `xiaomi` catalog exposes `mimo-v2.5-pro` and `mimo-v2.5` on both
the pay-as-you-go and Token Plan endpoints. Both routes use the OpenAI
Responses transport.

Both models use the documented 1,048,576-token context window and
131,072-token maximum output. `mimo-v2.5` supports image input while
`mimo-v2.5-pro` is text-only. Their selectable reasoning efforts are `none`
and `high`.

The retired MiMo V2 models are removed. `mimo-v2.5-pro-ultraspeed` is also
omitted because its official access is approval-based and capacity-limited,
not generally available through either built-in route.

## Sources

- Xiaomi MiMo, “Models”
  (`https://mimo.mi.com/docs/en-US/quick-start/summary/model`, updated
  2026-06-29).
- Xiaomi MiMo, “OpenAI Responses API”
  (`https://mimo.mi.com/docs/en-US/api/chat/responses`).
- Xiaomi MiMo, “Codex Configuration”
  (`https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration`).
- Xiaomi MiMo, “MiMo-V2.5-Pro-UltraSpeed”
  (`https://mimo.mi.com/models/zh-CN/mimo-v2.5-pro-ultraspeed`).

## Preserved Boundary

Intrinsic model capability remains separate from route availability. A model
with an official product page is not added to the general catalog unless the
documented endpoint is available without a separate approval workflow.

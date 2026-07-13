# NVIDIA model catalog

Holon snapshots models currently returned by NVIDIA's hosted
`https://integrate.api.nvidia.com/v1/models` endpoint and verifies their
capabilities against the corresponding Build.NVIDIA.com model cards. The
snapshot was verified on 2026-07-13.

The current built-in picker contains Nemotron 3 Super 120B, Kimi K2.6,
MiniMax M2.7, MiniMax M3, and GLM-5.2. The former Kimi K2.5, MiniMax M2.5,
and `z-ai/glm5` identifiers are no longer returned by the hosted model
directory and are not retained as defaults. Users may still configure an
arbitrary NVIDIA model ID explicitly.

Context windows and input modalities follow the individual NVIDIA model
cards. Kimi K2.6 and MiniMax M3 accept image input; the other current entries
are text-input models. All five cards describe reasoning behavior, but only
Nemotron, Kimi, and GLM document thinking controls. Holon therefore records
the intrinsic reasoning capability without inventing portable effort levels
for NVIDIA's route.

The hosted model cards do not publish a distinct maximum generation-token
limit for these API routes. Holon leaves the output limit unset rather than
reusing a context length or deployment example as an API constraint.

Sources:

- NVIDIA hosted model directory:
  `https://integrate.api.nvidia.com/v1/models`
- NVIDIA model catalog:
  `https://build.nvidia.com/models`
- Current hosted model cards:
  `https://build.nvidia.com/nvidia/nemotron-3-super-120b-a12b`
  `https://build.nvidia.com/moonshotai/kimi-k2.6`
  `https://build.nvidia.com/minimaxai/minimax-m2.7`
  `https://build.nvidia.com/minimaxai/minimax-m3`
  `https://build.nvidia.com/z-ai/glm-5.2`

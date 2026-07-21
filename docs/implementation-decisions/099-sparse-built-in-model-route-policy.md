# Sparse built-in model route policy

Built-in model metadata is owned by one canonical `provider/model` entry.
Endpoint availability is registered separately by `ModelRouteRef` with a
sparse route policy.

An omitted route field inherits the canonical model. A route capability
restriction may explicitly disable an intrinsic capability, but it cannot
enable a capability absent from the canonical model. The resolver applies such
restrictions as `endpoint_policy` constraints after selecting intrinsic
metadata, preserving the distinction between the winning model fact and the
endpoint restriction.

Legacy duplicated provider catalog entries are temporarily adapted into sparse
policies during construction so providers can migrate independently. New
route registrations use the sparse form directly. Model and route reference
serialization, legacy provider aliases, discovery cache behavior, and
model-level `models.catalog` overrides remain unchanged.

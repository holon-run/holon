# holon-model-catalog

Internal Holon model catalog and policy resolver. This crate owns validated
offline catalog loading, model aliases and preferred routes, model metadata
selection and policy resolution, and the models.dev adapter and review tools.
The application provides discovered metadata and resolved configuration.

The files in `data/` are the reviewed compile-time inputs to the catalog.
The independent tests run with:

```sh
cargo test --locked --manifest-path crates/holon-model-catalog/Cargo.toml
```

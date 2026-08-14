# Async misuse Miri fixture

This standalone crate intentionally violates `ScopedKey::set`'s safety
contract. It polls a future until the future has retained a scoped TLS
reference across suspension, lets `set` return, frees the referenced value,
and polls the future again.

Run it only under Miri, from the repository root:

```text
cargo +nightly miri run --locked --manifest-path miri-tests/async-misuse/Cargo.toml
```

Success for this fixture means that the command exits unsuccessfully and Miri
reports that the final access uses a dangling pointer whose allocation has been
freed. This is an expected-failure fixture, so it is deliberately separate from
the normal passing `cargo test` and `cargo miri test` suites.

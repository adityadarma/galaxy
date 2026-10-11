# Fuzzing

M7-05 has two `cargo-fuzz` targets:

- `edit_operations`: random Unicode document text, selections, and bounded edit
  operations. It asserts that the primary selection remains inside the document.
- `json_rpc`: arbitrary bytes through the JSON-RPC payload validator. Invalid
  input is expected to return an error and must not panic.

Install the tool once with `cargo install cargo-fuzz`, then run:

```sh
scripts/fuzz.sh edit_operations 300
scripts/fuzz.sh json_rpc 300
```

The targets are intentionally outside the main Cargo workspace because
`libfuzzer-sys` requires nightly Rust. The checked-in targets are smoke-tested
by compiling their source when `cargo-fuzz` is available; a long fuzz campaign
must run separately in CI/nightly.

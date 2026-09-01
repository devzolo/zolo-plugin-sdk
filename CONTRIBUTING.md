# Contributing

Thank you for contributing to the Zolo Plugin SDK.

## Before opening a pull request

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo test -p zolo-alloc --features host --test shim_roundtrip
cargo doc --workspace --no-deps
```

Changes to exported symbols, `PluginValue`, ownership transfer, allocator
behavior, or `ZOLO_PLUGIN_ABI_VERSION` are ABI-sensitive. Explain the
compatibility impact and include a focused regression test.

Never silently change the ABI number. A breaking ABI change must update the
constant, compatibility documentation, and changelog together.

By contributing, you agree that your contribution is licensed under MIT.

# Changelog

All notable changes to this project are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## [0.1.1] - 2026-09-01

### Fixed

- Made the repository's Rust formatting configuration self-contained so CI
  produces the same result outside the Zolo monorepo.

## [0.1.0] - 2026-09-01

### Added

- Public plugin API for functions, classes, handles, values, and declarations.
- Dynamic and static plugin registration support for Zolo plugin ABI 3.
- Shared host/plugin allocator bridge and linker export helper.
- Procedural macros used by plugin authors.

[0.1.1]: https://github.com/devzolo/zolo-plugin-sdk/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/devzolo/zolo-plugin-sdk/releases/tag/v0.1.0

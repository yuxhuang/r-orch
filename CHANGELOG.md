# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning:
[SemVer](https://semver.org/spec/v2.0.0.html). Add entries under `[Unreleased]` in the PR that makes the change.

## [Unreleased]


### Added
- `--yaml`, a global flag beside `--json`: every command that prints JSON can print the same data as one YAML document with every key and string double-quoted. The two flags together are a usage error, and `--json` output is byte-for-byte unchanged.

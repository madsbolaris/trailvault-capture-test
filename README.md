# Routebook: private session-capture demonstration

A small Rust CLI for browsing a fabricated hiking-route catalogue.
This branch is demonstration scaffolding, not production software.

```sh
cargo run -- list
cargo test
```

## Searching

`search` accepts any combination of `--name TEXT` (case-insensitive substring),
`--max-distance KM` (inclusive, non-negative) and
`--difficulty easy|moderate|hard`. Filters combine, and catalogue order is kept.

```sh
cargo run -- search --name fern
cargo run -- search --name fern --max-distance 8.5 --difficulty moderate
cargo run -- search --difficulty hard --max-distance 12
```

No matches prints nothing and exits 0. Invalid or missing arguments print an
error and usage to stderr and exit 2.

Routes are deliberately fictional. The catalogue has no external data sources,
accounts, customer information or runtime dependencies.

Feature PRs will demonstrate real Copilot CLI sessions and their Git-native
checkpoints. They are clearly labeled demonstrations; generated work and review
must not be mistaken for independent human contributions.

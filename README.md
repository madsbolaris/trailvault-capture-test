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

### Output and exit codes

- Success: matching routes are written to stdout, one per line, with exit 0
  and empty stderr. A search with no matches is still a success: it prints
  nothing and exits 0.
- Usage errors: nothing is written to stdout. stderr gets `error: <message>`
  followed by the usage text, and the exit code is 2. This covers unknown
  commands, unknown arguments, invalid values, missing values and repeated
  flags.
- Each flag may be given at most once; repeating one is an error
  (`error: --name was given more than once`).
- A flag must be followed by a value. If the next argument is another option
  name, the flag is reported as missing its value:

```sh
cargo run -- search --name --difficulty easy   # error: --name requires a value (exit 2)
cargo run -- search --name fern                # valid
cargo run -- search --name nomatch             # prints nothing, exit 0
```

Limitation: because `--name`, `--max-distance` and `--difficulty` are always
treated as option names, a name search for exactly one of those strings cannot
be expressed. Other values starting with `-` (such as `--name -x`) are accepted;
`--max-distance -1` is rejected as a negative distance.

## Exporting JSON

`export` takes the same filters as `search` (same parsing, matching and usage
errors) and prints the matches as one compact JSON array on a single line.

```sh
cargo run -q -- export
cargo run -q -- export --name fern --difficulty moderate
cargo run -q -- export --max-distance 8.5 | python3 -m json.tool
cargo run -q -- export --difficulty hard | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'
cargo run -q -- export --name nomatch          # []
```

### Schema

An array of objects, in catalogue order, each with exactly these keys:

| key           | type   | notes                                              |
|---------------|--------|----------------------------------------------------|
| `name`        | string | JSON-escaped (`"`, `\`, control characters as `\uXXXX`/short escapes); non-ASCII is emitted as UTF-8 |
| `distance_km` | number | finite, non-negative, plain decimal (never exponent notation); `7.0` is written `7` |
| `difficulty`  | string | `easy`, `moderate` or `hard`                       |

Example: `[{"name":"Fern Loop","distance_km":3.2,"difficulty":"easy"}]`

### Output and exit codes

- Success: exit 0, stdout is the JSON array plus a trailing newline, stderr is empty.
- No matches is a success: stdout is `[]`, exit 0 (unlike `search`, which prints nothing).
- Usage errors: exit 2, empty stdout, `error: <message>` and usage on stderr.
- Export failure (a route with a NaN, infinite or negative distance): exit 1,
  empty stdout, `error: export failed: <reason>` on stderr. No partial JSON is
  ever written. The built-in catalogue cannot trigger this; it guards the library.

Routes are deliberately fictional. The catalogue has no external data sources,
accounts, customer information or runtime dependencies.

Feature PRs will demonstrate real Copilot CLI sessions and their Git-native
checkpoints. They are clearly labeled demonstrations; generated work and review
must not be mistaken for independent human contributions.

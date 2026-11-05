# Route search design notes

How the `search` command is put together and why. For usage recipes see
[search-examples.md](search-examples.md); for the output contract see the
README. All route data is fictional.

## Layers

| Layer | File | Responsibility |
|---|---|---|
| Data | `src/lib.rs` (`catalogue`, `Route`, `Difficulty`) | A fixed, in-memory list of five routes in a stable order. |
| Filtering | `src/lib.rs` (`SearchFilter`, `search`) | Pure matching logic with no I/O and no knowledge of the CLI. |
| CLI | `src/main.rs` (`parse_search_args`, `main`) | Turns argv into a `SearchFilter`, prints results, maps errors to exit code 2. |
| Tests | `src/lib.rs`, `src/main.rs` unit tests; `tests/cli_search.rs` | Unit tests for filtering and parsing; black-box tests that run the built binary. |

The library has no dependency on the CLI, so the filter can be reused or tested
without spawning a process.

## `SearchFilter`

`SearchFilter` is a builder with three optional criteria:

- `name_contains(text)`: case-insensitive substring. The text is lowercased once
  when it is set, and each route name is lowercased at match time. An empty
  string matches every name.
- `max_distance_km(km)`: inclusive upper bound (`distance_km <= km`). NaN and
  negative values match nothing, because no comparison against them succeeds.
- `difficulty(d)`: exact match on the `Difficulty` enum.

An unset criterion matches everything (`Option::is_none_or`), and the criteria
are combined with AND. So `SearchFilter::new()` matches the whole catalogue and
each builder call can only narrow the result.

Setting the same criterion again replaces the previous value. The builder
allows this; the CLI forbids it (see below).

## `search`

`search(routes, filter)` returns `Vec<&Route>` borrowed from the input slice,
in the input's order. It never clones or reorders routes, which keeps output
deterministic and lets the tests check identity with `std::ptr::eq`.

## Argument parsing

`parse_search_args` walks the arguments after `search` with a peekable iterator:

1. Each argument must be one of `--name`, `--max-distance` or `--difficulty`.
   Anything else, including a bare positional word, is an `unknown argument`
   error.
2. A per-flag `seen` boolean rejects repeated flags
   (`--name was given more than once`). This avoids silently using whichever
   value came last.
3. The value is taken with `next_if`, which refuses another known flag name.
   That way `--name --difficulty easy` reports `--name requires a value`
   instead of searching for the text `--difficulty` and then failing on
   `easy`.
4. Values are validated before they reach the filter:
   - `--max-distance` must parse as a finite, non-negative `f64`. Stricter
     than the library, so a user sees an error instead of an empty result.
   - `--difficulty` is case-insensitive and must be `easy`, `moderate` or
     `hard`.
   - `--name` is accepted as-is, including the empty string and values that
     start with `-`.

Flags can appear in any order.

## Errors and exit codes

Parsing returns `Result<SearchFilter, String>`. `main` sends any error to
`fail`, which writes `error: <message>` and the usage text to stderr and exits
with code 2. Nothing goes to stdout on error, so piping results stays safe.

A search that matches nothing is not an error: it prints nothing and exits 0.

## Known limitations and trade-offs

- A name search for the exact text `--name`, `--max-distance` or
  `--difficulty` cannot be expressed, because those strings are always read as
  flags. There is no `--` escape. This was accepted to get a clear
  missing-value message.
- Matching uses `to_lowercase` and plain substring search: no accent folding,
  word boundaries or fuzzy matching.
- `search` scans the whole catalogue each time. That is fine for a fixed list of
  five routes; a larger catalogue would need an index.
- Argument parsing is hand-written rather than using a crate such as `clap`, so
  the binary keeps zero runtime dependencies.

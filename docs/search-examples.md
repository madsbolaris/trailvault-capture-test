# Route search examples

Quick recipes for the `search` command. All route data is fictional.

| Goal | Command |
|---|---|
| Find routes by name | `cargo run -- search --name fern` |
| Short walks only | `cargo run -- search --max-distance 5` |
| Easy routes | `cargo run -- search --difficulty easy` |
| Hard routes under 12 km | `cargo run -- search --difficulty hard --max-distance 12` |
| Everything combined | `cargo run -- search --name fern --max-distance 8.5 --difficulty moderate` |

Tips:

- Filters combine with AND, and results keep catalogue order.
- No matches is not an error: nothing is printed and the exit code is 0.
- Usage mistakes (missing values, repeated flags, bad difficulty) exit with code 2.

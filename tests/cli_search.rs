use std::process::{Command, Output};

const USAGE: &str = "Usage:\n  routebook list\n  routebook search [--name TEXT] [--max-distance KM] [--difficulty easy|moderate|hard]\n  routebook export [--name TEXT] [--max-distance KM] [--difficulty easy|moderate|hard]\n";

const FERN_LOOP: &str = "Fern Loop | 3.2 km | easy";
const NORTH_RIDGE: &str = "North Ridge | 11.5 km | hard";
const LAKE_CIRCUIT: &str = "Lake Circuit | 7.0 km | moderate";
const FERN_VALLEY: &str = "Fern Valley | 8.5 km | moderate";
const SUMMIT: &str = "Summit Traverse | 16.0 km | hard";
const ALL: [&str; 5] = [FERN_LOOP, NORTH_RIDGE, LAKE_CIRCUIT, FERN_VALLEY, SUMMIT];

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_routebook"))
        .args(args)
        .output()
        .expect("failed to run routebook")
}

fn assert_ok(args: &[&str], expected: &[&str]) {
    let out = run(args);
    let want: String = expected.iter().map(|r| format!("{r}\n")).collect();
    assert_eq!(out.status.code(), Some(0), "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "args {args:?}");
}

fn assert_usage_error(args: &[&str]) {
    let out = run(args);
    assert_eq!(out.status.code(), Some(2), "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "args {args:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.starts_with("error: "), "args {args:?}: {err:?}");
    assert!(err.ends_with(USAGE), "args {args:?}: {err:?}");
}

#[test]
fn list_preserves_catalogue_order() {
    assert_ok(&["list"], &ALL);
}

#[test]
fn search_without_filters_matches_list() {
    assert_ok(&["search"], &ALL);
}

#[test]
fn name_filter_is_case_insensitive_substring() {
    for name in ["fern", "FERN", "FeRn", "ern "] {
        assert_ok(&["search", "--name", name], &[FERN_LOOP, FERN_VALLEY]);
    }
    assert_ok(&["search", "--name", "RIDGE"], &[NORTH_RIDGE]);
}

#[test]
fn empty_name_matches_everything() {
    assert_ok(&["search", "--name", ""], &ALL);
}

#[test]
fn distance_filter_is_inclusive_and_ordered() {
    assert_ok(
        &["search", "--max-distance", "8.5"],
        &[FERN_LOOP, LAKE_CIRCUIT, FERN_VALLEY],
    );
    assert_ok(&["search", "--max-distance", "3.2"], &[FERN_LOOP]);
    assert_ok(&["search", "--max-distance", "16"], &ALL);
    assert_ok(
        &["search", "--max-distance", "8.4"],
        &[FERN_LOOP, LAKE_CIRCUIT],
    );
    assert_ok(&["search", "--max-distance", "1000"], &ALL);
}

#[test]
fn zero_distance_is_valid_and_matches_nothing() {
    assert_ok(&["search", "--max-distance", "0"], &[]);
}

#[test]
fn difficulty_filter_is_case_insensitive() {
    assert_ok(&["search", "--difficulty", "easy"], &[FERN_LOOP]);
    assert_ok(
        &["search", "--difficulty", "MODERATE"],
        &[LAKE_CIRCUIT, FERN_VALLEY],
    );
    assert_ok(&["search", "--difficulty", "Hard"], &[NORTH_RIDGE, SUMMIT]);
}

#[test]
fn filters_combine_in_any_flag_order() {
    assert_ok(
        &[
            "search",
            "--name",
            "fern",
            "--max-distance",
            "8.5",
            "--difficulty",
            "moderate",
        ],
        &[FERN_VALLEY],
    );
    assert_ok(
        &[
            "search",
            "--difficulty",
            "moderate",
            "--max-distance",
            "8.5",
            "--name",
            "fern",
        ],
        &[FERN_VALLEY],
    );
    assert_ok(
        &["search", "--difficulty", "hard", "--max-distance", "12"],
        &[NORTH_RIDGE],
    );
    assert_ok(
        &["search", "--name", "fern", "--difficulty", "easy"],
        &[FERN_LOOP],
    );
    assert_ok(
        &["search", "--name", "lake", "--max-distance", "7"],
        &[LAKE_CIRCUIT],
    );
}

#[test]
fn no_matches_prints_nothing_and_exits_zero() {
    assert_ok(&["search", "--name", "nonexistent"], &[]);
    assert_ok(&["search", "--name", "fern", "--difficulty", "hard"], &[]);
    assert_ok(&["search", "--max-distance", "3.1"], &[]);
}

#[test]
fn invalid_distances_are_rejected() {
    for v in [
        "abc", "-1", "-0.1", "NaN", "nan", "inf", "-inf", "infinity", "+inf", "", "1e999",
    ] {
        assert_usage_error(&["search", "--max-distance", v]);
    }
}

#[test]
fn invalid_distance_message_names_flag_and_value() {
    let out = run(&["search", "--max-distance", "abc"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--max-distance") && err.contains("abc"),
        "{err:?}"
    );
}

#[test]
fn unknown_difficulty_is_rejected() {
    for v in ["extreme", "", "easyy", "med", " easy"] {
        assert_usage_error(&["search", "--difficulty", v]);
    }
}

#[test]
fn missing_values_are_rejected() {
    for flag in ["--name", "--max-distance", "--difficulty"] {
        assert_usage_error(&["search", flag]);
    }
    assert_usage_error(&["search", "--difficulty", "easy", "--name"]);
}

#[test]
fn flag_following_a_flag_reports_missing_value() {
    let flags = ["--name", "--max-distance", "--difficulty"];
    for flag in flags {
        for next in flags {
            let out = run(&["search", flag, next, "easy"]);
            let err = String::from_utf8_lossy(&out.stderr);
            assert_usage_error(&["search", flag, next, "easy"]);
            assert!(
                err.starts_with(&format!("error: {flag} requires a value\n")),
                "{flag} {next}: {err:?}"
            );
        }
        let out = run(&["search", flag, "--difficulty"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("requires a value"), "{flag}: {err:?}");
    }
    let out = run(&["search", "--name", "--max-distance", "10"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("error: --name requires a value\n"),
        "{err:?}"
    );
}

#[test]
fn negative_distance_is_still_a_value_error() {
    let out = run(&["search", "--max-distance", "-1"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("error: invalid --max-distance '-1'"),
        "{err:?}"
    );
}

#[test]
fn duplicate_flags_are_rejected() {
    assert_usage_error(&["search", "--name", "a", "--name", "b"]);
    assert_usage_error(&["search", "--max-distance", "5", "--max-distance", "6"]);
    assert_usage_error(&["search", "--difficulty", "easy", "--difficulty", "hard"]);
    assert_usage_error(&[
        "search",
        "--name",
        "a",
        "--difficulty",
        "easy",
        "--name",
        "a",
    ]);
}

#[test]
fn unknown_flags_and_positionals_are_rejected() {
    assert_usage_error(&["search", "--bogus"]);
    assert_usage_error(&["search", "--bogus", "x"]);
    assert_usage_error(&["search", "fern"]);
    assert_usage_error(&["search", "--name", "fern", "--bogus"]);
}

#[test]
fn bad_commands_are_rejected() {
    assert_usage_error(&[]);
    assert_usage_error(&["bogus"]);
    assert_usage_error(&["list", "extra"]);
    assert_usage_error(&["list", "--name", "fern"]);
}

#[test]
fn errors_do_not_print_partial_results() {
    assert_usage_error(&["search", "--name", "fern", "--max-distance", "bad"]);
}

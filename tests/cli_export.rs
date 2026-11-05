use std::process::{Command, Output};

const FERN_LOOP: &str = r#"{"name":"Fern Loop","distance_km":3.2,"difficulty":"easy"}"#;
const NORTH_RIDGE: &str = r#"{"name":"North Ridge","distance_km":11.5,"difficulty":"hard"}"#;
const LAKE_CIRCUIT: &str = r#"{"name":"Lake Circuit","distance_km":7,"difficulty":"moderate"}"#;
const FERN_VALLEY: &str = r#"{"name":"Fern Valley","distance_km":8.5,"difficulty":"moderate"}"#;
const SUMMIT: &str = r#"{"name":"Summit Traverse","distance_km":16,"difficulty":"hard"}"#;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_routebook"))
        .args(args)
        .output()
        .expect("failed to run routebook")
}

fn assert_export(args: &[&str], expected: &[&str]) {
    let out = run(args);
    let want = format!("[{}]\n", expected.join(","));
    assert_eq!(out.status.code(), Some(0), "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "args {args:?}");
}

fn assert_export_usage_error(args: &[&str]) {
    let out = run(args);
    assert_eq!(out.status.code(), Some(2), "args {args:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "args {args:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("error: "), "args {args:?}: {err:?}");
    assert!(err.contains("Usage:"), "args {args:?}: {err:?}");
}

#[test]
fn unfiltered_export_is_one_array_in_catalogue_order() {
    assert_export(
        &["export"],
        &[FERN_LOOP, NORTH_RIDGE, LAKE_CIRCUIT, FERN_VALLEY, SUMMIT],
    );
}

#[test]
fn filtered_exports_match_search_semantics() {
    assert_export(&["export", "--name", "FERN"], &[FERN_LOOP, FERN_VALLEY]);
    assert_export(
        &["export", "--max-distance", "8.5"],
        &[FERN_LOOP, LAKE_CIRCUIT, FERN_VALLEY],
    );
    assert_export(&["export", "--difficulty", "Hard"], &[NORTH_RIDGE, SUMMIT]);
    assert_export(
        &[
            "export",
            "--difficulty",
            "moderate",
            "--max-distance",
            "8.5",
            "--name",
            "fern",
        ],
        &[FERN_VALLEY],
    );
}

#[test]
fn empty_export_prints_empty_array_and_succeeds() {
    assert_export(&["export", "--name", "nonexistent"], &[]);
    assert_export(&["export", "--max-distance", "0"], &[]);
}

#[test]
fn export_output_is_valid_single_line_json_array() {
    let out = run(&["export"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with('[') && text.ends_with("]\n"));
    assert_eq!(text.matches('\n').count(), 1);
}

#[test]
fn export_parse_errors_exit_2_with_stderr() {
    for args in [
        &["export", "--max-distance", "abc"][..],
        &["export", "--max-distance", "-1"],
        &["export", "--difficulty", "extreme"],
        &["export", "--name"],
        &["export", "--name", "a", "--name", "b"],
        &["export", "--bogus"],
        &["export", "fern"],
    ] {
        assert_export_usage_error(args);
    }
}

#[test]
fn export_error_message_names_flag_and_value() {
    let out = run(&["export", "--max-distance", "abc"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("error: invalid --max-distance 'abc'"),
        "{err:?}"
    );
}

#[test]
fn list_and_search_output_is_unchanged() {
    let list = run(&["list"]);
    let search = run(&["search"]);
    let expected = "Fern Loop | 3.2 km | easy\nNorth Ridge | 11.5 km | hard\nLake Circuit | 7.0 km | moderate\nFern Valley | 8.5 km | moderate\nSummit Traverse | 16.0 km | hard\n";
    for out in [&list, &search] {
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
        assert!(out.stderr.is_empty());
    }
    let filtered = run(&["search", "--difficulty", "easy"]);
    assert_eq!(
        String::from_utf8_lossy(&filtered.stdout),
        "Fern Loop | 3.2 km | easy\n"
    );
}

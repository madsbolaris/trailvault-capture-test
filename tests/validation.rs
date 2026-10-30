use routebook::{Difficulty, Route, ValidationIssue, ValidationReason, catalogue, validate_routes};

fn route(name: &'static str, distance_km: f64) -> Route {
    Route {
        name,
        distance_km,
        difficulty: Difficulty::Easy,
    }
}

fn issue(index: usize, reason: ValidationReason) -> ValidationIssue {
    ValidationIssue { index, reason }
}

#[test]
fn empty_and_default_catalogues_are_valid() {
    assert_eq!(validate_routes(&[]), Ok(()));
    assert_eq!(validate_routes(&catalogue()), Ok(()));
}

#[test]
fn multiple_issues_keep_original_indexes_and_order() {
    let routes = [
        route("Ok", 1.0),
        route(" \t", 2.0),
        route("ok", f64::INFINITY),
        route("", 0.0),
        route("Fine", 4.0),
        route("FINE ", -1.0),
    ];
    assert_eq!(
        validate_routes(&routes),
        Err(vec![
            issue(1, ValidationReason::BlankName),
            issue(2, ValidationReason::DuplicateName { first_index: 0 }),
            issue(2, ValidationReason::NonFiniteDistance),
            issue(3, ValidationReason::BlankName),
            issue(3, ValidationReason::NonPositiveDistance),
            issue(5, ValidationReason::DuplicateName { first_index: 4 }),
            issue(5, ValidationReason::NonPositiveDistance),
        ])
    );
}

#[test]
fn blank_names_are_never_reported_as_duplicates() {
    let routes = [route("", 1.0), route("   ", 1.0), route("\n", 1.0)];
    assert_eq!(
        validate_routes(&routes),
        Err(vec![
            issue(0, ValidationReason::BlankName),
            issue(1, ValidationReason::BlankName),
            issue(2, ValidationReason::BlankName),
        ])
    );
}

#[test]
fn duplicates_ignore_case_and_surrounding_whitespace_but_not_inner() {
    let routes = [
        route("Fern Loop", 1.0),
        route("  fern loop\t", 1.0),
        route("FERN LOOP", 1.0),
        route("Fern  Loop", 1.0),
    ];
    assert_eq!(
        validate_routes(&routes),
        Err(vec![
            issue(1, ValidationReason::DuplicateName { first_index: 0 }),
            issue(2, ValidationReason::DuplicateName { first_index: 0 }),
        ])
    );
}

#[test]
fn unicode_lowercasing_detects_duplicates() {
    let routes = [
        route("ÉCOLE", 1.0),
        route(" école ", 1.0),
        route("ΟΔΟΣ", 1.0),
        route("οδος", 1.0),
    ];
    assert_eq!(
        validate_routes(&routes),
        Err(vec![
            issue(1, ValidationReason::DuplicateName { first_index: 0 }),
            issue(3, ValidationReason::DuplicateName { first_index: 2 }),
        ])
    );
}

// Only lowercasing is applied, not Unicode normalization (no dependencies allowed).
#[test]
fn canonically_equivalent_forms_are_not_normalised() {
    let routes = [route("\u{e9}cole", 1.0), route("e\u{301}cole", 1.0)];
    assert_eq!(validate_routes(&routes), Ok(()));
}

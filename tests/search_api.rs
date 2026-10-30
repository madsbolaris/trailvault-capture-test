use routebook::{Difficulty, Route, SearchFilter, catalogue, search};

fn names(found: &[&Route]) -> Vec<&'static str> {
    found.iter().map(|route| route.name).collect()
}

#[test]
fn results_borrow_from_input_slice() {
    let routes = catalogue();
    let found = search(&routes, &SearchFilter::new().difficulty(Difficulty::Hard));
    assert_eq!(found.len(), 2);
    for route in found {
        assert!(
            routes
                .iter()
                .any(|candidate| std::ptr::eq(candidate, route))
        );
    }
}

#[test]
fn results_keep_input_order() {
    let mut routes = catalogue();
    routes.reverse();
    let found = search(&routes, &SearchFilter::new().name_contains("fern"));
    assert_eq!(names(&found), ["Fern Valley", "Fern Loop"]);
}

#[test]
fn finite_thresholds_are_inclusive() {
    let routes = catalogue();
    let at = search(&routes, &SearchFilter::new().max_distance_km(8.5));
    assert_eq!(names(&at), ["Fern Loop", "Lake Circuit", "Fern Valley"]);
    let below = search(&routes, &SearchFilter::new().max_distance_km(8.4));
    assert_eq!(names(&below), ["Fern Loop", "Lake Circuit"]);
    assert!(search(&routes, &SearchFilter::new().max_distance_km(0.0)).is_empty());
    let all = search(&routes, &SearchFilter::new().max_distance_km(f64::MAX));
    assert_eq!(all.len(), routes.len());
}

#[test]
fn nonfinite_and_negative_bounds_match_nothing() {
    let routes = catalogue();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        let filter = SearchFilter::new().max_distance_km(bad);
        assert!(search(&routes, &filter).is_empty(), "{bad}");
    }
}

#[test]
fn filters_compose() {
    let routes = catalogue();
    let filter = SearchFilter::new()
        .name_contains("FERN")
        .max_distance_km(8.5)
        .difficulty(Difficulty::Moderate);
    assert_eq!(names(&search(&routes, &filter)), ["Fern Valley"]);

    let bad = filter.max_distance_km(f64::INFINITY);
    assert!(search(&routes, &bad).is_empty());
}

#[test]
fn no_filter_returns_everything() {
    let routes = catalogue();
    let found = search(&routes, &SearchFilter::default());
    assert_eq!(found.len(), routes.len());
    assert!(found.iter().zip(&routes).all(|(a, b)| std::ptr::eq(*a, b)));
}

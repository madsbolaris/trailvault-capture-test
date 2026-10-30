use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difficulty {
    Easy,
    Moderate,
    Hard,
}

impl fmt::Display for Difficulty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Easy => "easy",
            Self::Moderate => "moderate",
            Self::Hard => "hard",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub name: &'static str,
    pub distance_km: f64,
    pub difficulty: Difficulty,
}

pub fn catalogue() -> Vec<Route> {
    vec![
        Route {
            name: "Fern Loop",
            distance_km: 3.2,
            difficulty: Difficulty::Easy,
        },
        Route {
            name: "North Ridge",
            distance_km: 11.5,
            difficulty: Difficulty::Hard,
        },
        Route {
            name: "Lake Circuit",
            distance_km: 7.0,
            difficulty: Difficulty::Moderate,
        },
        Route {
            name: "Fern Valley",
            distance_km: 8.5,
            difficulty: Difficulty::Moderate,
        },
        Route {
            name: "Summit Traverse",
            distance_km: 16.0,
            difficulty: Difficulty::Hard,
        },
    ]
}

pub fn display_route(route: &Route) -> String {
    format!(
        "{} | {:.1} km | {}",
        route.name, route.distance_km, route.difficulty
    )
}

/// Composable route filter; unset criteria match everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchFilter {
    name_contains: Option<String>,
    max_distance_km: Option<f64>,
    difficulty: Option<Difficulty>,
}

impl SearchFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Case-insensitive name substring. An empty string matches every name.
    pub fn name_contains(mut self, text: impl Into<String>) -> Self {
        self.name_contains = Some(text.into().to_lowercase());
        self
    }

    /// Inclusive maximum distance. NaN, infinite or negative values match no routes.
    pub fn max_distance_km(mut self, km: f64) -> Self {
        self.max_distance_km = Some(km);
        self
    }

    pub fn difficulty(mut self, difficulty: Difficulty) -> Self {
        self.difficulty = Some(difficulty);
        self
    }

    pub fn matches(&self, route: &Route) -> bool {
        self.name_contains
            .as_ref()
            .is_none_or(|text| route.name.to_lowercase().contains(text.as_str()))
            && self
                .max_distance_km
                .is_none_or(|max| max.is_finite() && max >= 0.0 && route.distance_km <= max)
            && self.difficulty.is_none_or(|d| route.difficulty == d)
    }
}

/// Returns matching routes borrowed from `routes`, in their original order.
pub fn search<'a>(routes: &'a [Route], filter: &SearchFilter) -> Vec<&'a Route> {
    routes
        .iter()
        .filter(|route| filter.matches(route))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(found: &[&Route]) -> Vec<&'static str> {
        found.iter().map(|route| route.name).collect()
    }

    #[test]
    fn empty_filter_returns_all_in_order() {
        let routes = catalogue();
        let found = search(&routes, &SearchFilter::new());
        assert_eq!(found.len(), routes.len());
        assert!(found.iter().zip(&routes).all(|(a, b)| std::ptr::eq(*a, b)));
    }

    #[test]
    fn name_is_case_insensitive_substring() {
        let routes = catalogue();
        let found = search(&routes, &SearchFilter::new().name_contains("FERN"));
        assert_eq!(names(&found), ["Fern Loop", "Fern Valley"]);
    }

    #[test]
    fn max_distance_is_inclusive() {
        let routes = catalogue();
        let found = search(&routes, &SearchFilter::new().max_distance_km(8.5));
        assert_eq!(names(&found), ["Fern Loop", "Lake Circuit", "Fern Valley"]);
    }

    #[test]
    fn difficulty_filters() {
        let routes = catalogue();
        let found = search(&routes, &SearchFilter::new().difficulty(Difficulty::Hard));
        assert_eq!(names(&found), ["North Ridge", "Summit Traverse"]);
    }

    #[test]
    fn combined_filters_all_apply() {
        let routes = catalogue();
        let filter = SearchFilter::new()
            .name_contains("fern")
            .max_distance_km(8.5)
            .difficulty(Difficulty::Moderate);
        assert_eq!(names(&search(&routes, &filter)), ["Fern Valley"]);
        let none = filter.max_distance_km(8.4);
        assert!(search(&routes, &none).is_empty());
    }

    #[test]
    fn invalid_distances_match_nothing() {
        let routes = catalogue();
        assert!(search(&routes, &SearchFilter::new().max_distance_km(f64::NAN)).is_empty());
        assert!(search(&routes, &SearchFilter::new().max_distance_km(-1.0)).is_empty());
    }

    #[test]
    fn positive_infinity_matches_nothing() {
        let routes = catalogue();
        let filter = SearchFilter::new().max_distance_km(f64::INFINITY);
        assert!(search(&routes, &filter).is_empty());
    }

    #[test]
    fn negative_infinity_matches_nothing() {
        let routes = catalogue();
        let filter = SearchFilter::new().max_distance_km(f64::NEG_INFINITY);
        assert!(search(&routes, &filter).is_empty());
    }

    #[test]
    fn zero_and_f64_max_are_finite_bounds() {
        let routes = catalogue();
        assert!(search(&routes, &SearchFilter::new().max_distance_km(0.0)).is_empty());
        let found = search(&routes, &SearchFilter::new().max_distance_km(f64::MAX));
        assert_eq!(found.len(), routes.len());
    }

    #[test]
    fn invalid_distance_with_other_filters_matches_nothing() {
        let routes = catalogue();
        for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -1.0] {
            let filter = SearchFilter::new()
                .name_contains("fern")
                .difficulty(Difficulty::Moderate)
                .max_distance_km(bad);
            assert!(search(&routes, &filter).is_empty(), "{bad}");
        }
        let ok = SearchFilter::new()
            .name_contains("fern")
            .difficulty(Difficulty::Moderate)
            .max_distance_km(f64::MAX);
        assert_eq!(names(&search(&routes, &ok)), ["Fern Valley"]);
    }

    #[test]
    fn catalogue_is_small_and_deterministic() {
        assert_eq!(catalogue().len(), 5);
        assert_eq!(catalogue()[0].name, "Fern Loop");
    }

    #[test]
    fn route_display_is_readable() {
        assert_eq!(display_route(&catalogue()[0]), "Fern Loop | 3.2 km | easy");
    }
}

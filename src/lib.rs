use std::collections::HashMap;
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

    /// Inclusive maximum distance. NaN or negative values match no routes.
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
                .is_none_or(|max| route.distance_km <= max)
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

/// Why a route failed catalogue validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationReason {
    BlankName,
    /// Name matches (trimmed, case-insensitively) the route at `first_index`.
    DuplicateName {
        first_index: usize,
    },
    /// Distance is NaN or infinite.
    NonFiniteDistance,
    /// Distance is finite but zero or negative.
    NonPositiveDistance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub index: usize,
    pub reason: ValidationReason,
}

/// Checks every route and returns all issues, ordered by route index.
/// An empty catalogue is valid.
pub fn validate_routes(routes: &[Route]) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for (index, route) in routes.iter().enumerate() {
        let name = route.name.trim();
        if name.is_empty() {
            issues.push(ValidationIssue {
                index,
                reason: ValidationReason::BlankName,
            });
        } else if let Some(&first_index) = seen.get(&name.to_lowercase()) {
            issues.push(ValidationIssue {
                index,
                reason: ValidationReason::DuplicateName { first_index },
            });
        } else {
            seen.insert(name.to_lowercase(), index);
        }

        if !route.distance_km.is_finite() {
            issues.push(ValidationIssue {
                index,
                reason: ValidationReason::NonFiniteDistance,
            });
        } else if route.distance_km <= 0.0 {
            issues.push(ValidationIssue {
                index,
                reason: ValidationReason::NonPositiveDistance,
            });
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn valid_and_empty_catalogues_pass() {
        assert_eq!(validate_routes(&catalogue()), Ok(()));
        assert_eq!(validate_routes(&[]), Ok(()));
    }

    #[test]
    fn reports_multiple_simultaneous_issues() {
        let routes = [
            route("  ", 1.0),
            route("A", -2.0),
            route(" a ", f64::NAN),
            route("B", 0.0),
        ];
        assert_eq!(
            validate_routes(&routes),
            Err(vec![
                issue(0, ValidationReason::BlankName),
                issue(1, ValidationReason::NonPositiveDistance),
                issue(2, ValidationReason::DuplicateName { first_index: 1 }),
                issue(2, ValidationReason::NonFiniteDistance),
                issue(3, ValidationReason::NonPositiveDistance),
            ])
        );
    }

    #[test]
    fn duplicates_ignore_case_and_whitespace() {
        let routes = [
            route("Fern", 1.0),
            route(" FERN\t", 2.0),
            route("fern", 3.0),
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
    fn duplicates_use_unicode_lowercasing() {
        let routes = [route("ÉCOLE", 1.0), route("école", 1.0)];
        assert_eq!(
            validate_routes(&routes),
            Err(vec![issue(
                1,
                ValidationReason::DuplicateName { first_index: 0 }
            )])
        );
    }

    #[test]
    fn invalid_distances_are_typed() {
        let routes = [
            route("zero", 0.0),
            route("neg", -1.0),
            route("nan", f64::NAN),
            route("inf", f64::INFINITY),
            route("neginf", f64::NEG_INFINITY),
        ];
        assert_eq!(
            validate_routes(&routes),
            Err(vec![
                issue(0, ValidationReason::NonPositiveDistance),
                issue(1, ValidationReason::NonPositiveDistance),
                issue(2, ValidationReason::NonFiniteDistance),
                issue(3, ValidationReason::NonFiniteDistance),
                issue(4, ValidationReason::NonFiniteDistance),
            ])
        );
    }

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
    fn catalogue_is_small_and_deterministic() {
        assert_eq!(catalogue().len(), 5);
        assert_eq!(catalogue()[0].name, "Fern Loop");
    }

    #[test]
    fn route_display_is_readable() {
        assert_eq!(display_route(&catalogue()[0]), "Fern Loop | 3.2 km | easy");
    }
}

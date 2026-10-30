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

/// Reason a route set could not be exported as JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportError {
    /// The route's distance is NaN or infinite.
    NonFiniteDistance { name: String },
    /// The route's distance is below zero.
    NegativeDistance { name: String, distance_km: f64 },
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteDistance { name } => {
                write!(formatter, "route {name:?} has a non-finite distance")
            }
            Self::NegativeDistance { name, distance_km } => {
                write!(
                    formatter,
                    "route {name:?} has a negative distance ({distance_km})"
                )
            }
        }
    }
}

impl std::error::Error for ExportError {}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if c < '\u{20}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Exports routes as a compact JSON array with `name`, `distance_km` and
/// `difficulty` keys, preserving input order. Fails before producing any
/// output if a distance is non-finite or negative.
pub fn export_json(routes: &[&Route]) -> Result<String, ExportError> {
    let mut out = String::from("[");
    for (index, route) in routes.iter().enumerate() {
        let distance = route.distance_km;
        if !distance.is_finite() {
            return Err(ExportError::NonFiniteDistance {
                name: route.name.to_owned(),
            });
        }
        if distance < 0.0 {
            return Err(ExportError::NegativeDistance {
                name: route.name.to_owned(),
                distance_km: distance,
            });
        }
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        push_json_string(&mut out, route.name);
        // Adding 0.0 normalises -0.0 to 0.0.
        out.push_str(&format!(",\"distance_km\":{}", distance + 0.0));
        out.push_str(",\"difficulty\":");
        push_json_string(&mut out, &route.difficulty.to_string());
        out.push('}');
    }
    out.push(']');
    Ok(out)
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
    fn catalogue_is_small_and_deterministic() {
        assert_eq!(catalogue().len(), 5);
        assert_eq!(catalogue()[0].name, "Fern Loop");
    }

    #[test]
    fn route_display_is_readable() {
        assert_eq!(display_route(&catalogue()[0]), "Fern Loop | 3.2 km | easy");
    }

    fn route(name: &'static str, distance_km: f64) -> Route {
        Route {
            name,
            distance_km,
            difficulty: Difficulty::Easy,
        }
    }

    #[test]
    fn exports_catalogue() {
        let routes = catalogue();
        let refs: Vec<&Route> = routes.iter().collect();
        assert_eq!(
            export_json(&refs).unwrap(),
            concat!(
                r#"[{"name":"Fern Loop","distance_km":3.2,"difficulty":"easy"},"#,
                r#"{"name":"North Ridge","distance_km":11.5,"difficulty":"hard"},"#,
                r#"{"name":"Lake Circuit","distance_km":7,"difficulty":"moderate"},"#,
                r#"{"name":"Fern Valley","distance_km":8.5,"difficulty":"moderate"},"#,
                r#"{"name":"Summit Traverse","distance_km":16,"difficulty":"hard"}]"#
            )
        );
    }

    #[test]
    fn exports_empty_selection() {
        assert_eq!(export_json(&[]).unwrap(), "[]");
    }

    #[test]
    fn escapes_special_and_control_characters() {
        let r = route("a\"b\\c\td\re\nf\u{08}\u{0c}\u{01}\u{1f}é🌲", -0.0);
        let json = export_json(&[&r]).unwrap();
        assert_eq!(
            json,
            r#"[{"name":"a\"b\\c\td\re\nf\b\f\u0001\u001fé🌲","distance_km":0,"difficulty":"easy"}]"#
        );
    }

    #[test]
    fn escapes_every_control_character() {
        let name: &'static str = Box::leak(
            (0u8..0x20)
                .map(char::from)
                .collect::<String>()
                .into_boxed_str(),
        );
        let json = export_json(&[&route(name, 1.0)]).unwrap();
        let body = json
            .strip_prefix(r#"[{"name":""#)
            .and_then(|s| s.strip_suffix(r#"","distance_km":1,"difficulty":"easy"}]"#))
            .unwrap();
        assert!(body.chars().all(|c| c >= '\u{20}'));
        assert!(body.contains("\\u0000") && body.contains("\\u001f") && body.contains("\\u000b"));
    }

    #[test]
    fn extreme_distances_are_plain_json_numbers() {
        for (value, text) in [
            (f64::MAX, f64::MAX.to_string()),
            (f64::MIN_POSITIVE, f64::MIN_POSITIVE.to_string()),
            (1e-7, "0.0000001".to_string()),
            (0.1 + 0.2, "0.30000000000000004".to_string()),
        ] {
            let json = export_json(&[&route("X", value)]).unwrap();
            assert!(json.contains(&format!("\"distance_km\":{text},")), "{json}");
        }
    }

    #[test]
    fn rejects_nonfinite_distances() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let r = route("Bad", value);
            let err = export_json(&[&r]).unwrap_err();
            assert_eq!(err, ExportError::NonFiniteDistance { name: "Bad".into() });
            assert!(err.to_string().contains("non-finite"));
        }
    }

    #[test]
    fn rejects_negative_distance() {
        let good = route("Good", 1.0);
        let bad = route("Bad", -0.5);
        let err = export_json(&[&good, &bad]).unwrap_err();
        assert_eq!(
            err,
            ExportError::NegativeDistance {
                name: "Bad".into(),
                distance_km: -0.5
            }
        );
        let _: &dyn std::error::Error = &err;
        assert!(err.to_string().contains("negative"));
    }
}

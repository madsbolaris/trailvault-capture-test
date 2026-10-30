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

#[cfg(test)]
mod tests {
    use super::*;

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

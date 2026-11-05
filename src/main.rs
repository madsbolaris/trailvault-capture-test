use routebook::{
    Difficulty, SearchFilter, ValidationIssue, ValidationReason, catalogue, display_route, search,
    validate_routes,
};

const USAGE: &str = "Usage:\n  routebook list\n  routebook validate\n  routebook search [--name TEXT] [--max-distance KM] [--difficulty easy|moderate|hard]";

fn parse_difficulty(value: &str) -> Result<Difficulty, String> {
    match value.to_ascii_lowercase().as_str() {
        "easy" => Ok(Difficulty::Easy),
        "moderate" => Ok(Difficulty::Moderate),
        "hard" => Ok(Difficulty::Hard),
        _ => Err(format!(
            "invalid difficulty '{value}': expected easy, moderate or hard"
        )),
    }
}

fn parse_distance(value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(km) if km.is_finite() && km >= 0.0 => Ok(km),
        _ => Err(format!(
            "invalid --max-distance '{value}': expected a non-negative number of kilometers"
        )),
    }
}

fn parse_search_args(args: &[String]) -> Result<SearchFilter, String> {
    let mut filter = SearchFilter::new();
    let (mut name, mut distance, mut difficulty) = (false, false, false);
    let mut iter = args.iter().peekable();
    while let Some(flag) = iter.next() {
        let seen = match flag.as_str() {
            "--name" => &mut name,
            "--max-distance" => &mut distance,
            "--difficulty" => &mut difficulty,
            other => return Err(format!("unknown argument '{other}'")),
        };
        if *seen {
            return Err(format!("{flag} was given more than once"));
        }
        *seen = true;
        let value = iter
            .next_if(|next| !matches!(next.as_str(), "--name" | "--max-distance" | "--difficulty"))
            .ok_or_else(|| format!("{flag} requires a value"))?;
        filter = match flag.as_str() {
            "--name" => filter.name_contains(value.as_str()),
            "--max-distance" => filter.max_distance_km(parse_distance(value)?),
            _ => filter.difficulty(parse_difficulty(value)?),
        };
    }
    Ok(filter)
}

fn describe(issue: &ValidationIssue) -> String {
    let problem = match issue.reason {
        ValidationReason::BlankName => "blank name".to_string(),
        ValidationReason::DuplicateName { first_index } => {
            format!("duplicate name (first seen at route {first_index})")
        }
        ValidationReason::NonFiniteDistance => "distance is not finite".to_string(),
        ValidationReason::NonPositiveDistance => "distance must be positive".to_string(),
    };
    format!("route {}: {problem}", issue.index)
}

fn fail(message: &str) -> ! {
    eprintln!("error: {message}\n{USAGE}");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "list" && rest.is_empty() => {
            for route in catalogue() {
                println!("{}", display_route(&route));
            }
        }
        Some((command, rest)) if command == "search" => {
            let filter = parse_search_args(rest).unwrap_or_else(|message| fail(&message));
            let routes = catalogue();
            for route in search(&routes, &filter) {
                println!("{}", display_route(route));
            }
        }
        Some((command, rest)) if command == "validate" && rest.is_empty() => {
            let routes = catalogue();
            match validate_routes(&routes) {
                Ok(()) => println!("catalogue valid: {} routes checked", routes.len()),
                Err(issues) => {
                    for issue in &issues {
                        eprintln!("{}", describe(issue));
                    }
                    std::process::exit(1);
                }
            }
        }
        Some((command, _)) if command == "validate" => fail("validate takes no arguments"),
        Some((command, _)) if command == "list" => fail("list takes no arguments"),
        Some((command, _)) => fail(&format!("unknown command '{command}'")),
        None => fail("missing command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<SearchFilter, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_search_args(&args)
    }

    #[test]
    fn parses_combined_flags() {
        let expected = SearchFilter::new()
            .name_contains("fern")
            .max_distance_km(8.5)
            .difficulty(Difficulty::Moderate);
        let parsed = parse(&[
            "--difficulty",
            "Moderate",
            "--name",
            "fern",
            "--max-distance",
            "8.5",
        ]);
        assert_eq!(parsed, Ok(expected));
    }

    #[test]
    fn rejects_bad_input() {
        for args in [
            &["--max-distance", "abc"][..],
            &["--max-distance", "-1"],
            &["--max-distance", "NaN"],
            &["--difficulty", "extreme"],
            &["--name"],
            &["--name", "--difficulty", "easy"],
            &["--name", "a", "--name", "b"],
            &["--bogus", "x"],
            &["fern"],
        ] {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
    }
}

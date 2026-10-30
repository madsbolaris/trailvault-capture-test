use routebook::{catalogue, display_route};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["list"] {
        for route in catalogue() {
            println!("{}", display_route(&route));
        }
    } else {
        eprintln!("Usage: routebook list");
        std::process::exit(2);
    }
}

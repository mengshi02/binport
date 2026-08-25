use std::{env, fs, process};

fn main() {
    let mut args = env::args().skip(1);
    let Some(pattern) = args.next() else {
        eprintln!("usage: rg PATTERN FILE");
        process::exit(2);
    };
    let Some(display_path) = args.next() else {
        eprintln!("usage: rg PATTERN FILE");
        process::exit(2);
    };
    let path = env::var("BINPORT_DEMO_LOG").unwrap_or_else(|_| display_path.clone());
    let contents = fs::read_to_string(&path).unwrap_or_else(|error| {
        eprintln!("rg: {display_path}: {error}");
        process::exit(2);
    });
    for (index, line) in contents.lines().enumerate() {
        if line.contains(&pattern) {
            println!("{display_path}:{}:{line}", index + 1);
        }
    }
}

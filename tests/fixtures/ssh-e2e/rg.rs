use std::{env, ffi::OsStr, fs, io::{Read, Write}, process};

fn main() {
    if env::current_exe()
        .ok()
        .is_some_and(|path| path.file_name() == Some(OsStr::new("btm")))
    {
        println!("TTY_READY");
        std::io::stdout().flush().unwrap();
        let mut input = [0_u8; 1];
        std::io::stdin().read_exact(&mut input).unwrap();
        assert_eq!(input[0], b'q');
    }

    let mut args = env::args().skip(1);
    let Some(pattern) = args.next() else {
        eprintln!("usage: rg PATTERN FILE");
        process::exit(2);
    };
    let Some(display_path) = args.next() else {
        eprintln!("usage: rg PATTERN FILE");
        process::exit(2);
    };
    let path = env::var("BINPORT_E2E_LOG").unwrap_or_else(|_| display_path.clone());
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

#![feature(rustc_private)]

fn main() {
    if let Err(err) = varies::run_from_env() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

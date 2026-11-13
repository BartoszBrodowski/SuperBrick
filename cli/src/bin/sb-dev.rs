//! `sb-dev`: SuperBrick's development and benchmarking commands (also reachable as `sb <cmd>`).
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(&args)
}

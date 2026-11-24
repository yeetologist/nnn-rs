use std::process::ExitCode;

fn main() -> ExitCode {
    nnn::locale::init();
    ExitCode::from(nnn::startup::run(std::env::args_os().collect()))
}

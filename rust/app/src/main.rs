use std::process::ExitCode;

fn main() -> ExitCode {
    nnn::locale::init();
    let code = nnn::startup::run(std::env::args_os().collect());
    nnn::platform::signal_exit_complete();
    ExitCode::from(code)
}

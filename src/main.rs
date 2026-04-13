use std::process::ExitCode;

fn main() -> ExitCode {
    match open_switcher::daemon::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            if let Some(hint) = error.linux_input_setup_hint() {
                eprintln!("{hint}");
            }
            ExitCode::FAILURE
        }
    }
}

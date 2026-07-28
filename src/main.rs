use open_switcher::error::{InputSafetyError, SwitcherError};
use std::process::ExitCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entrypoint {
    Daemon,
    XtestGuardian,
}

fn select_entrypoint<I, S>(args: I) -> Result<Entrypoint, InputSafetyError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    match (args.next(), args.next()) {
        (None, None) => Ok(Entrypoint::Daemon),
        (Some(argument), None) if argument.as_ref() == "--internal-xtest-guardian-v1" => {
            Ok(Entrypoint::XtestGuardian)
        }
        _ => Err(InputSafetyError::InvalidEntrypoint),
    }
}

fn should_print_linux_input_setup_hint(entrypoint: Entrypoint) -> bool {
    entrypoint == Entrypoint::Daemon
}

fn run_entrypoint(entrypoint: Entrypoint) -> Result<(), SwitcherError> {
    match entrypoint {
        Entrypoint::Daemon => open_switcher::daemon::run(),
        Entrypoint::XtestGuardian => open_switcher::daemon::run_internal_xtest_guardian_v1(),
    }
}

fn main() -> ExitCode {
    let entrypoint = match select_entrypoint(std::env::args()) {
        Ok(entrypoint) => entrypoint,
        Err(error) => {
            eprintln!("Error: {error:?}");
            return ExitCode::FAILURE;
        }
    };

    match run_entrypoint(entrypoint) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            if should_print_linux_input_setup_hint(entrypoint) {
                if let Some(hint) = error.linux_input_setup_hint() {
                    eprintln!("{hint}");
                }
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_selects_normal_daemon_entrypoint() {
        assert_eq!(
            select_entrypoint(["open-switcher-daemon"]),
            Ok(Entrypoint::Daemon),
        );
    }

    #[test]
    fn exact_hidden_argument_selects_xtest_guardian_entrypoint() {
        assert_eq!(
            select_entrypoint(["open-switcher-daemon", "--internal-xtest-guardian-v1",]),
            Ok(Entrypoint::XtestGuardian),
        );
    }

    #[test]
    fn entrypoint_rejects_extra_or_unknown_arguments() {
        assert!(select_entrypoint([
            "open-switcher-daemon",
            "--internal-xtest-guardian-v1",
            "extra",
        ])
        .is_err());
        assert!(select_entrypoint(["open-switcher-daemon", "--help"]).is_err());
    }

    #[test]
    fn entrypoint_never_prints_linux_input_setup_hint_for_internal_mode() {
        assert!(should_print_linux_input_setup_hint(Entrypoint::Daemon));
        assert!(!should_print_linux_input_setup_hint(
            Entrypoint::XtestGuardian
        ));
    }
}

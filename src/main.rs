use std::process::ExitCode;

use clap::Parser;

use fulmar::api::ApiError;
use fulmar::cli::Cli;
use fulmar::session::SessionError;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match fulmar::commands::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fulmar: {err:#}");
            exit_code(&err)
        }
    }
}

fn init_tracing(verbose: u8) {
    let filter = match verbose {
        0 => "fulmar=warn",
        1 => "fulmar=debug",
        _ => "debug",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

/// Exit codes are part of the contract (see `fulmar --help`):
/// 3 = the session capability is gone and only `fulmar login` can fix
/// it (agents branch on this); 4 = subject not found; 1 = everything
/// else at runtime. clap emits 2 for usage errors on its own.
fn exit_code(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        if let Some(api) = cause.downcast_ref::<ApiError>() {
            match api {
                ApiError::SessionExpired | ApiError::Session(SessionError::Missing { .. }) => {
                    return ExitCode::from(3);
                }
                ApiError::Api { status, kind, .. } => {
                    if *status == 404 || kind.ends_with("NotFound") {
                        return ExitCode::from(4);
                    }
                    return ExitCode::FAILURE;
                }
                ApiError::Http(_)
                | ApiError::Decode(_)
                | ApiError::Session(_)
                | ApiError::Unexpected(_)
                | ApiError::Identifier(_) => return ExitCode::FAILURE,
            }
        }
        if let Some(session) = cause.downcast_ref::<SessionError>() {
            match session {
                SessionError::Missing { .. } => return ExitCode::from(3),
                SessionError::Corrupt { .. } | SessionError::Io { .. } | SessionError::NoHome => {
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    ExitCode::FAILURE
}

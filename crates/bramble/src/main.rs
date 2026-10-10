use std::process::ExitCode;

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("whoami") | None => {
            println!(
                "bramble {} (protocol v{}) — a pet-bank that teaches payments",
                env!("CARGO_PKG_VERSION"),
                bramble_core::PROTOCOL_VERSION
            );
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            ExitCode::FAILURE
        }
    }
}

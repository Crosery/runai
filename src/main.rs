use clap::Parser;

fn main() {
    tracing_subscriber::fmt::init();

    let cli = runai::cli::Cli::parse();
    if let Err(e) = runai::cli::run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

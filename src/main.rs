use clap::Parser;

fn main() {
    tracing_subscriber::fmt::init();

    let cli = skill_manager::cli::Cli::parse();
    if let Err(e) = skill_manager::cli::run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

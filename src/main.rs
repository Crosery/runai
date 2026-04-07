use clap::Parser;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cli = runai::cli::Cli::parse();

    // Spawn background update check (non-blocking, CLI mode only)
    let is_cli_mode = cli.command.is_some();
    let update_handle = if is_cli_mode {
        let data_dir = runai::core::paths::data_dir();
        Some(std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().ok();
            if let Some(rt) = rt {
                rt.block_on(runai::core::updater::check_for_update(data_dir));
            }
        }))
    } else {
        None
    };

    let result = runai::cli::run(cli);

    // After CLI run completes, show update notification if available
    if is_cli_mode {
        if let Some(handle) = update_handle {
            let _ = handle.join();
        }
        let data_dir = runai::core::paths::data_dir();
        if let Some(notification) = runai::core::updater::update_notification(&data_dir) {
            eprintln!("\n{notification}");
        }
    }

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

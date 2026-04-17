use clap::Parser;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cli = runai::cli::Cli::parse();

    // Spawn background update check for all modes (CLI + TUI). The check is
    // bounded by HTTP timeouts in `http_client`, so the join below cannot
    // block the process indefinitely.
    let data_dir = runai::core::paths::data_dir();
    let update_handle = {
        let dir = data_dir.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().ok();
            if let Some(rt) = rt {
                rt.block_on(runai::core::updater::check_for_update(dir));
            }
        })
    };

    let result = runai::cli::run(cli);

    // After CLI/TUI run completes, surface any pending update notification.
    // TUI has already restored the terminal by this point, so eprintln is visible.
    let _ = update_handle.join();
    if let Some(notification) = runai::core::updater::update_notification(&data_dir) {
        eprintln!("\n{notification}");
    }

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

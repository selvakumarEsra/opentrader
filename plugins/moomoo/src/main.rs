/*
 * The plugin process. OpenTrader starts it while the plugin is enabled and starts it again when it stops, so it
 * exits when the session ends; stdout and stderr go to the plugin's log in Settings > Plugins. Run by the user with a
 * command (`backfill`, `record`), it fills the bar store instead (src/cli.rs).
 */
fn main() {
    // The user's own commands: `backfill` and `record` (src/cli.rs).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let options = match opentrader_moomoo::cli::parse(&args) {
            Ok(o) => o,
            // Asked for: the usage, and a success.
            Err(e) if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help" | "help")) => {
                println!("{e}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
            eprintln!("Cannot start: {e}");
            std::process::exit(1);
        });
        match runtime.block_on(opentrader_moomoo::cli::run(options)) {
            Ok(true) => std::process::exit(0),
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    let (addr, plugin, token) = opentrader_plugin_kit::session::from_env(opentrader_moomoo::ID);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Cannot start: {e}");
            std::process::exit(1);
        }
    };
    // Exit at once: the OpenD connection's tasks need no orderly stop.
    match runtime.block_on(opentrader_moomoo::run_session(&addr, &plugin, &token)) {
        Ok(()) => {
            println!("[info] OpenTrader closed the connection");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

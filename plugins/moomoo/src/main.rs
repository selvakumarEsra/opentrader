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
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let (Some(addr), Some(token)) = (env("OPENTRADER_PLUGIN_ADDR"), env("OPENTRADER_PLUGIN_TOKEN")) else {
        eprintln!("OPENTRADER_PLUGIN_ADDR and OPENTRADER_PLUGIN_TOKEN are not set: OpenTrader starts this program itself");
        std::process::exit(2);
    };
    let plugin = env("OPENTRADER_PLUGIN_ID").unwrap_or_else(|| "moomoo".into());
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

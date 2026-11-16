/*
 * The plugin process. OpenTrader starts it while the plugin is enabled and starts it again when it stops; stdout and
 * stderr go to the plugin's log in Settings > Plugins.
 */
fn main() {
    if std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "-h" | "--help" | "help")) {
        println!("opentrader-zerodha: OpenTrader's Zerodha Kite Connect data plugin. OpenTrader starts it; set the KITE_* credentials in Settings > Plugins > Zerodha.\nAlso, unattended: opentrader-zerodha {}  (credentials from KITE_* or --env FILE)", opentrader_plugin_kit::backfill::USAGE);
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "backfill") {
        std::process::exit(opentrader_plugin_kit::backfill::main(opentrader_zerodha::Zerodha::default(), &args[1..]));
    }
    opentrader_plugin_kit::feed::main(opentrader_zerodha::Zerodha::default());
}

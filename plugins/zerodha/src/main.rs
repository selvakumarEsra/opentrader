/*
 * The plugin process. OpenTrader starts it while the plugin is enabled and starts it again when it stops; stdout and
 * stderr go to the plugin's log in Settings > Plugins.
 */
fn main() {
    if std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "-h" | "--help" | "help")) {
        println!("opentrader-zerodha: OpenTrader's Zerodha Kite Connect data plugin. OpenTrader starts it; set the KITE_* credentials in Settings > Plugins > Zerodha.");
        return;
    }
    opentrader_plugin_kit::feed::main(opentrader_zerodha::Zerodha::default());
}

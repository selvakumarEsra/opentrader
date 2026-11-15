/*
 * The plugin process. OpenTrader starts it while the plugin is enabled and starts it again when it stops; stdout and
 * stderr go to the plugin's log in Settings > Plugins.
 */
fn main() {
    if std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "-h" | "--help" | "help")) {
        println!("opentrader-dhan: OpenTrader's Dhan data plugin. OpenTrader starts it; set the DHAN_* credentials in Settings > Plugins > Dhan.");
        return;
    }
    opentrader_plugin_kit::feed::main(opentrader_dhan::Dhan::default());
}

# Plugin kit

`opentrader-plugin-kit`: the parts the repo's Rust plugins share, a library only (no program).

| Module | |
|---|---|
| `rpc`, `session` | the app's JSON-lines protocol and the `hello` handshake ([PROTOCOL.md](../PROTOCOL.md)) |
| `store` | the bar store: SQLite in `opentrader-<plugin>` in the cache folder, or ClickHouse `<plugin>_bars` / `<plugin>_covered`; name it with `set_plugin` |
| `pace` | at most N requests per window |
| `catalog` | an instrument list kept on disk, with ranked search |
| `calendar` | Indian exchange sessions (IST), to skip ranges with no trading |
| `history` | `data.bars` from the store, asking a `Source` only for what is missing |
| `live` | ticks to 1-minute bars, written to the store |
| `totp` | RFC 6238 codes from a base32 secret |
| `feed` | a whole data plugin over a `Provider` (search, symbol info, bars, live, "Always record") |

Used by `moomoo` (rpc, pace, store) and by `zerodha` and `dhan` (all of it). `cargo test` runs its tests.

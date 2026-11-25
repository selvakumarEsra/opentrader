# Plugin kit

`opentrader-plugin-kit`: the parts the repo's Rust plugins share, a library only (no program).

| Module | |
|---|---|
| `rpc`, `session` | the app's JSON-lines protocol, the `hello` handshake ([PROTOCOL.md](../PROTOCOL.md)), and `serve`: notifications in order, each request after the notifications before it |
| `store` | the bar store: SQLite in `opentrader-<plugin>` in the cache folder, or ClickHouse `<plugin>_bars` / `<plugin>_covered` (the settings' `backend_of`, `StoreSettings`); every plugin names itself with `set_plugin` first |
| `pace` | at most N requests per window |
| `catalog` | an instrument list kept on disk, with ranked search |
| `calendar` | an exchange's sessions and zone, to skip ranges with no trading (`Calendar::india`) |
| `history` | `data.bars` from the store, asking a `Source` only for what is missing |
| `live` | ticks to 1-minute bars, written to the store |
| `token` | a broker's access token, kept in memory and in a private file, made again when refused |
| `socket` | a broker's WebSocket stream of ticks, connected again after a drop (feature `socket`) |
| `totp` | RFC 6238 codes from a base32 secret |
| `backfill` | the `backfill` command of a `Provider`'s program: fill the store unattended |
| `feed` | a whole data plugin over a `Provider` (search, symbol info, bars, live, "Always record"); the provider gives its calendar and currency |

Used by `moomoo` (rpc, session, pace, store, the history window) and by `zerodha` and `dhan` (all of it, with `socket`). `cargo test` runs its tests.

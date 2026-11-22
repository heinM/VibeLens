This is the upstream `mysql_async` 0.37.1 crate (crates.io sources; the `[[test]]` targets
that point at files not shipped here are removed from `Cargo.toml`). VibeLens uses it for
MySQL connections and changes four things, each marked `VibeLens patch` in the source:

1. `src/opts/mod.rs`, `Opts::get_capabilities`: `CLIENT_LOCAL_FILES` and
   `CLIENT_MULTI_STATEMENTS` are no longer advertised. The server then refuses
   `LOAD DATA LOCAL INFILE`, and refuses a `COM_QUERY` that holds more than one statement, so a
   statement that was checked and approved cannot carry a second one. Upstream sets both
   unconditionally and has no option to turn them off.
2. `src/conn/routines/helpers.rs`, `handle_local_infile`: a `LOCAL INFILE` request from the
   server (0xFB) is never answered with a file. No handler (per-connection or global) is
   consulted; the connection is closed and the query fails with `LocalInfileError::NoHandler`.
   Only a hostile or broken server sends this request once (1) is in place.
3. `src/conn/mod.rs`, `continue_caching_sha2_password_auth`: `caching_sha2_password` full
   authentication sends the password only over TLS. Upstream, without TLS, asks the server for
   its RSA public key and encrypts the password with whatever key comes back (an active
   attacker can supply their own), and treats a Unix socket as secure. Both are refused with
   the new `DriverError::InsecureFullAuth`; nothing is sent.
4. `src/conn/mod.rs`, `perform_auth_switch`: an auth switch to `mysql_clear_password` is
   answered only when the caller enabled the plugin *and* the connection is encrypted.

VibeLens adds its own checks on top (TLS required for passwords to other machines, the
cleartext plugin enabled only for RDS IAM tokens over verified TLS); see
`crates/vibelens-core/src/mysql/` and `docs/REMOTE_ENGINES_AND_REDSHIFT.md`, section "MySQL".
Drop this copy only for an upstream release that offers all four as options.

Helpers left unused by (2) and (3) are removed so the crate builds without warnings:
`Conn::is_socket`, `Conn::write_bytes` (`src/conn/mod.rs`) and `Stream::is_socket`
(`src/io/mod.rs`), and the `crypto` import in `src/conn/mod.rs`.

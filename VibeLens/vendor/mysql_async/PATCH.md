This is the upstream `mysql_async` 0.37.1 crate (crates.io sources; the `[[test]]` targets
that point at files not shipped here are removed from `Cargo.toml`). VibeLens uses it for
MySQL connections and changes the following, each marked `VibeLens patch` in the source:

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
5. No server bytes can abort the process (release builds use `panic = "abort"`):
   - `perform_auth_switch`: a switch to `client_ed25519` (which, without the crate's
     `client_ed25519` feature, reaches an unconditional panic in `mysql_common`'s scramble) or
     to MariaDB `parsec` (an `unwrap` in `continue_parsec_auth`) fails with
     `DriverError::UnknownAuthPlugin`; a second switch returns `UnexpectedPacket` instead of
     `unreachable!`; the parsec `unwrap` returns `InvalidParsecSalt`.
   - `src/conn/routines/helpers.rs`, `handle_result_set`: a result header announcing 0 or more
     than `MAX_COLUMNS` (4096, MySQL's own limit) columns is refused.
   - `src/queryable/stmt.rs`, `read_column_defs`: definitions are read one at a time, the
     pre-allocation is capped at `MAX_COLUMNS`, and all definitions of one result set together
     may take at most `MAX_COLUMN_DEFS_BYTES` (16 MiB; each is one packet of up to
     `max_allowed_packet`, so 4096 of them could otherwise reach hundreds of GiB). The now
     unused `Conn::read_packets` is removed.
6. Third review, the whole class (server bytes reaching a panic):
   - Fixed: `src/queryable/mod.rs`, `is_last_result_set_packet` indexed `packet[0]` (twice) and
     panicked on a zero-length row packet; it uses `packet.first()`.
   - Guarded before a `mysql_common` call: `src/conn/mod.rs`, `handle_handshake` refuses a
     handshake whose auth-plugin-data length has the high bit set, because `HandshakePacket`
     computes `len as i8 - 8`, which overflows (a panic where overflow checks are on) for
     0x80..=0x87. Real servers send 21.
   - Swept and shown unreachable from server bytes, for the paths VibeLens uses (connect,
     handshake, TLS upgrade, every auth plugin and switch, COM_QUERY text protocol, result-set
     header, column definitions, rows, OK/ERR/EOF, multi-result sets, COM_PING,
     COM_RESET_CONNECTION, COM_QUIT, drop and cleanup):
     - driver state invariants, not data: `io/mod.rs` `Endpoint`/stream/codec `unwrap` and
       `unreachable!` (11), `io/write_packet.rs` codec `expect` (3), `io/tls/rustls_io.rs`
       (3), `conn/mod.rs` `take_stream` (stream present after a successful write),
       `use_pending_result` (`Err` handled first), `ssl_opts_and_connector().expect`
       (`CLIENT_SSL` is requested only with TLS options), `drop_result`'s `unreachable!`
       (`set_pending_result(None)?` returns early on a stored error);
     - guarded by a check on the same line: `continue_parsec_auth` `&packet[1..]` (after
       `first() == 0x01`), `caching_sha2` `packet.get(1)`;
     - code paths VibeLens does not take: `read_settings` `row.unwrap()` (both settings are
       preset in the options, `prefer_socket` is off), `drop_result`'s `Arc::try_unwrap`
       (`Taken` results exist only through the stream API), `helpers.rs` `stmt.unwrap()`
       (metadata skipping is binary protocol only), the `query_first`/`exec_first` family's
       `expect` (`query.rs`, `queryable/mod.rs`, not called);
     - `mysql_common`, on server bytes: the packet codec bounds each read by
       `max_allowed_packet` (64 MiB, set by VibeLens) before reserving; `read_lenenc_int`'s
       `unreachable!` is exhaustive; `read_varlen_uint` and text `Value` parsing check for an
       empty buffer before `[0]`; OK/ERR/column/row parsers return errors through `ParseBuf`;
       `scramble_native`/`scramble_sha256` take any nonce; `Seq` pre-allocation from a length
       is used only by GTID, bulk-execute and binlog parsers (not reached); `Row::unwrap`,
       indexing and `get` panics are avoided (VibeLens uses `unwrap_raw`); `ErrPacket`'s
       panicking accessors are not called; the old-password and parsec scramble `unwrap`s and
       the ed25519 panic sit behind plugins refused in (5) and by `secure_auth`; the version
       regex `unwrap`s are guarded: the driver refuses a version string that is not plain ASCII before the parse (item 7), and with ASCII input the matched digits fit `u16`.
   Release builds have no overflow checks; the handshake length is the only server-controlled
   arithmetic found that overflows, and it is guarded. `tests/mysql_hostile.rs`
   (`fuzz::a_corrupting_server_never_panics_or_hangs_the_engine`) drives a seeded corrupting
   server through every stage; it found the handshake overflow, and it finds the
   `is_last_result_set_packet` panic within ~100 iterations when that fix is reverted.

VibeLens adds its own checks on top (TLS required for passwords to other machines, the
cleartext plugin enabled only for RDS IAM tokens over verified TLS); see
`crates/vibelens-core/src/mysql/` and `docs/REMOTE_ENGINES_AND_REDSHIFT.md`, section "MySQL".
Drop this copy only for an upstream release that offers all of these.

Helpers left unused by (2) and (3) are removed so the crate builds without warnings:
`Conn::is_socket`, `Conn::write_bytes` (`src/conn/mod.rs`) and `Stream::is_socket`
(`src/io/mod.rs`), and the `crypto` import in `src/conn/mod.rs`.

7. **Handshake version string, and the drop-time drain.** `mysql_common` matches the server
   version with a Unicode-aware `\d` and converts the capture with `btoi(..).unwrap()`, so a
   greeting whose version contains a non-ASCII decimal digit (for example U+0663) panicked. The
   greeting is read before TLS, so this needed no credentials. `handle_handshake` now refuses a
   version that is not plain ASCII. Separately, the cleanup task spawned when a connection is
   dropped mid-result is limited to 5 seconds.

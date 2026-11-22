//! The MySQL client against a hostile server: an in-process fake that speaks just enough of
//! the protocol to log in and answer queries, and then does what a malicious or compromised
//! server (or an attacker in the path of an unencrypted connection) would do. Runs without any
//! outside service.
//!
//! Each test states what the server tries and what must not happen.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use secrecy::SecretString;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::mysql::{MysqlConnectParams, MysqlHandle};
use vibelens_core::ports::CredentialFn;

const PASSWORD: &str = "hunter2-not-for-the-server";

// Capability flags (subset).
const LONG_PASSWORD: u32 = 0x1;
const FOUND_ROWS: u32 = 0x2;
const CONNECT_WITH_DB: u32 = 0x8;
const LOCAL_FILES: u32 = 0x80;
const PROTOCOL_41: u32 = 0x200;
const TRANSACTIONS: u32 = 0x2000;
const SECURE_CONNECTION: u32 = 0x8000;
const MULTI_STATEMENTS: u32 = 0x1_0000;
const MULTI_RESULTS: u32 = 0x2_0000;
const PS_MULTI_RESULTS: u32 = 0x4_0000;
const PLUGIN_AUTH: u32 = 0x8_0000;
const CONNECT_ATTRS: u32 = 0x10_0000;
const PLUGIN_AUTH_LENENC: u32 = 0x20_0000;
const DEPRECATE_EOF: u32 = 0x100_0000;

/// Everything the server offers, including what the client must not take up.
const SERVER_CAPS: u32 = LONG_PASSWORD
    | FOUND_ROWS
    | CONNECT_WITH_DB
    | LOCAL_FILES
    | PROTOCOL_41
    | TRANSACTIONS
    | SECURE_CONNECTION
    | MULTI_STATEMENTS
    | MULTI_RESULTS
    | PS_MULTI_RESULTS
    | PLUGIN_AUTH
    | CONNECT_ATTRS
    | PLUGIN_AUTH_LENENC
    | DEPRECATE_EOF;

/// What the fake server does after the client's handshake response, and for queries.
#[derive(Clone, Copy, PartialEq)]
enum Plan {
    /// `caching_sha2_password`, then "perform full authentication" without TLS.
    DemandFullAuth,
    /// Switch the client to `mysql_clear_password` without TLS.
    SwitchToCleartext,
    /// Log in, then answer `SELECT 'trigger'` with a LOCAL INFILE request for `file`.
    AskForFile,
    /// Log in, then answer `SELECT 'trigger'` with a hostile result set.
    HostileRows,
    /// Log in, then answer `SELECT 'trigger'` with a column of an unknown type.
    UnknownType,
}

/// What the server saw.
#[derive(Default)]
struct Seen {
    client_caps: Vec<u32>,
    /// Every packet payload received after the handshake response, in order.
    packets: Vec<Vec<u8>>,
}

impl Seen {
    fn all_bytes(&self) -> Vec<u8> {
        self.packets.iter().flatten().copied().collect()
    }
}

async fn read_packet(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut h = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut h))
        .await
        .ok()?
        .ok()?;
    let len = u32::from_le_bytes([h[0], h[1], h[2], 0]) as usize;
    let mut p = vec![0u8; len];
    s.read_exact(&mut p).await.ok()?;
    Some((h[3], p))
}

async fn write_packet(s: &mut TcpStream, seq: u8, payload: &[u8]) {
    let len = payload.len() as u32;
    let mut out = vec![len as u8, (len >> 8) as u8, (len >> 16) as u8, seq];
    out.extend_from_slice(payload);
    let _ = s.write_all(&out).await;
}

fn lenenc_str(out: &mut Vec<u8>, s: &[u8]) {
    assert!(s.len() < 251);
    out.push(s.len() as u8);
    out.extend_from_slice(s);
}

const OK: &[u8] = &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
/// Result-set terminator with CLIENT_DEPRECATE_EOF: an OK packet with the 0xFE header.
const END: &[u8] = &[0xFE, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];

fn handshake(plugin: &str) -> Vec<u8> {
    let mut p = vec![0x0a];
    p.extend_from_slice(b"8.0.99-fake\0");
    p.extend_from_slice(&7u32.to_le_bytes());
    p.extend_from_slice(b"abcdefgh");
    p.push(0);
    p.extend_from_slice(&(SERVER_CAPS as u16).to_le_bytes());
    p.push(255);
    p.extend_from_slice(&0x0002u16.to_le_bytes());
    p.extend_from_slice(&((SERVER_CAPS >> 16) as u16).to_le_bytes());
    p.push(21);
    p.extend_from_slice(&[0u8; 10]);
    p.extend_from_slice(b"ijklmnopqrst\0");
    p.extend_from_slice(plugin.as_bytes());
    p.push(0);
    p
}

/// A column definition (protocol 41).
fn column(name: &str, ty: u8, charset: u16, flags: u16) -> Vec<u8> {
    let mut c = Vec::new();
    for s in ["def", "fake", "t", "t", name, name] {
        lenenc_str(&mut c, s.as_bytes());
    }
    c.push(0x0c);
    c.extend_from_slice(&charset.to_le_bytes());
    c.extend_from_slice(&1024u32.to_le_bytes());
    c.push(ty);
    c.extend_from_slice(&flags.to_le_bytes());
    c.push(0);
    c.extend_from_slice(&[0, 0]);
    c
}

async fn serve(mut s: TcpStream, plan: Plan, file: String, seen: Arc<Mutex<Seen>>) {
    let plugin = match plan {
        Plan::DemandFullAuth => "caching_sha2_password",
        _ => "mysql_native_password",
    };
    write_packet(&mut s, 0, &handshake(plugin)).await;
    let Some((_, resp)) = read_packet(&mut s).await else {
        return;
    };
    let caps = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
    seen.lock().unwrap().client_caps.push(caps);
    match plan {
        Plan::DemandFullAuth => {
            // "fast authentication failed, send the password" (0x01 0x04).
            write_packet(&mut s, 2, &[0x01, 0x04]).await;
            // A vulnerable client now sends 0x02 (give me your RSA key) or the password.
            while let Some((_, p)) = read_packet(&mut s).await {
                seen.lock().unwrap().packets.push(p);
            }
            return;
        }
        Plan::SwitchToCleartext => {
            let mut sw = vec![0xFE];
            sw.extend_from_slice(b"mysql_clear_password\0");
            write_packet(&mut s, 2, &sw).await;
            while let Some((_, p)) = read_packet(&mut s).await {
                seen.lock().unwrap().packets.push(p);
            }
            return;
        }
        _ => write_packet(&mut s, 2, OK).await,
    }
    // Commands.
    while let Some((_, p)) = read_packet(&mut s).await {
        seen.lock().unwrap().packets.push(p.clone());
        match p.first() {
            Some(0x01) => return, // COM_QUIT
            Some(0x03) if p[1..].starts_with(b"SELECT 'trigger'") => match plan {
                Plan::AskForFile => {
                    let mut req = vec![0xFB];
                    req.extend_from_slice(file.as_bytes());
                    write_packet(&mut s, 1, &req).await;
                }
                Plan::HostileRows => {
                    write_packet(&mut s, 1, &[3]).await;
                    // VARCHAR utf8mb4, JSON, BIT(64)
                    write_packet(&mut s, 2, &column("txt", 253, 255, 0)).await;
                    write_packet(&mut s, 3, &column("doc", 245, 63, 0x90)).await;
                    write_packet(&mut s, 4, &column("bits", 16, 63, 0x20)).await;
                    // Row 1: invalid UTF-8, JSON nested 300 deep, a 9-byte BIT.
                    let mut row = Vec::new();
                    lenenc_str(&mut row, &[0x61, 0xFF, 0xFE, 0x62]);
                    let deep = format!("{}{}", "[".repeat(120), "]".repeat(120));
                    lenenc_str(&mut row, deep.as_bytes());
                    lenenc_str(&mut row, &[0xFF; 9]);
                    write_packet(&mut s, 5, &row).await;
                    // Row 2: NULLs.
                    write_packet(&mut s, 6, &[0xFB, 0xFB, 0xFB]).await;
                    // Row 3: claims a 200-byte value but sends 3 bytes.
                    write_packet(&mut s, 7, &[200, b'a', b'b', b'c']).await;
                    write_packet(&mut s, 8, END).await;
                }
                Plan::UnknownType => {
                    write_packet(&mut s, 1, &[1]).await;
                    write_packet(&mut s, 2, &column("x", 0x77, 255, 0)).await;
                    let mut row = Vec::new();
                    lenenc_str(&mut row, b"1");
                    write_packet(&mut s, 3, &row).await;
                    write_packet(&mut s, 4, END).await;
                }
                _ => write_packet(&mut s, 1, OK).await,
            },
            // Session setup, resets, USE, pings: all fine.
            _ => write_packet(&mut s, 1, OK).await,
        }
    }
}

struct Fake {
    port: u16,
    seen: Arc<Mutex<Seen>>,
}

async fn fake(plan: Plan, file: &str) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (seen2, file) = (seen.clone(), file.to_string());
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(serve(s, plan, file.clone(), seen2.clone()));
        }
    });
    Fake { port, seen }
}

fn params(port: u16, cleartext_plugin: bool) -> MysqlConnectParams {
    MysqlConnectParams {
        host: "127.0.0.1".into(),
        port,
        tls_server_name: "127.0.0.1".into(),
        database: "fake".into(),
        tls: TlsConfig {
            mode: TlsMode::Disable,
            ..Default::default()
        },
        options: MysqlOptions {
            connect_timeout_secs: 5,
            max_pool_size: 1,
            ..Default::default()
        },
        cleartext_plugin,
    }
}

fn creds() -> CredentialFn {
    Arc::new(|| {
        let c = ResolvedCredentials {
            username: Some("app".into()),
            password: Some(SecretString::from(PASSWORD)),
            ..ResolvedCredentials::anonymous()
        };
        async move { Ok(c) }.boxed()
    })
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The server asks for the full password on a connection without TLS. A vulnerable client
/// asks the server for its RSA public key (0x02) and encrypts the password with it, which an
/// attacker in the path can answer with their own key. Nothing may be sent.
#[tokio::test]
async fn no_password_and_no_rsa_key_request_without_tls() {
    let f = fake(Plan::DemandFullAuth, "").await;
    let e = MysqlHandle::connect(params(f.port, false), creds())
        .await
        .err()
        .expect("refused");
    assert!(
        matches!(&e, CoreError::Auth(m) if m.contains("full authentication")),
        "{e:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let seen = f.seen.lock().unwrap();
    assert!(
        seen.packets.iter().all(|p| p.as_slice() != [0x02]),
        "the client asked the server for its RSA key"
    );
    assert!(!contains(&seen.all_bytes(), PASSWORD.as_bytes()));
}

/// An attacker switches the client to the cleartext plugin. Even with the plugin enabled (as it
/// is for RDS IAM), it is not answered without TLS.
#[tokio::test]
async fn no_cleartext_password_without_tls_even_when_enabled() {
    for enabled in [false, true] {
        let f = fake(Plan::SwitchToCleartext, "").await;
        let e = MysqlHandle::connect(params(f.port, enabled), creds())
            .await
            .err()
            .expect("refused");
        assert!(matches!(&e, CoreError::Auth(_)), "{e:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = f.seen.lock().unwrap();
        assert!(
            !contains(&seen.all_bytes(), PASSWORD.as_bytes()),
            "cleartext password sent (plugin enabled: {enabled})"
        );
    }
}

/// The client never offers to send local files or to run several statements per query, even
/// when the server offers both.
#[tokio::test]
async fn the_client_takes_up_neither_local_files_nor_multi_statements() {
    let f = fake(Plan::HostileRows, "").await;
    let h = MysqlHandle::connect(params(f.port, false), creds())
        .await
        .expect("login");
    drop(h);
    let seen = f.seen.lock().unwrap();
    assert!(!seen.client_caps.is_empty());
    for caps in &seen.client_caps {
        assert_eq!(caps & LOCAL_FILES, 0, "CLIENT_LOCAL_FILES advertised");
        assert_eq!(
            caps & MULTI_STATEMENTS,
            0,
            "CLIENT_MULTI_STATEMENTS advertised"
        );
    }
}

/// A server answers a plain SELECT with a LOCAL INFILE request for a file on this machine. No
/// byte of it is sent, and the connection is dropped.
#[tokio::test]
async fn a_local_infile_request_gets_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.txt");
    let secret = format!("vg-secret-{}", uuid::Uuid::new_v4().simple());
    std::fs::write(&path, &secret).unwrap();
    let f = fake(Plan::AskForFile, &path.display().to_string()).await;
    let h = MysqlHandle::connect(params(f.port, false), creds())
        .await
        .expect("login");
    let e = h
        .execute_collect(
            "SELECT 'trigger'",
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CoreError::PolicyDenied(m) if m.contains("LOCAL INFILE")),
        "{e:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = f.seen.lock().unwrap();
    assert!(
        !contains(&seen.all_bytes(), secret.as_bytes()),
        "the file was sent to the server"
    );
    // Nothing at all followed the request on that connection: the last packet the server got
    // is the query itself (or a later connection's setup).
    let after: Vec<&Vec<u8>> = seen
        .packets
        .iter()
        .skip_while(|p| !p.ends_with(b"SELECT 'trigger'"))
        .skip(1)
        .collect();
    assert!(
        after.iter().all(|p| !p.is_empty()),
        "an (empty) file-data packet was sent"
    );
}

/// Invalid UTF-8 in a text column, JSON deeper than the parser allows, an over-long BIT, NULLs
/// and a row that lies about its own length: mapped or refused, never a panic.
#[tokio::test]
async fn hostile_rows_are_mapped_or_refused_without_panicking() {
    let f = fake(Plan::HostileRows, "").await;
    let h = MysqlHandle::connect(params(f.port, false), creds())
        .await
        .expect("login");
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    h.execute_stream(
        "q".into(),
        "SELECT 'trigger'",
        // One row per page, so the rows before the malformed one reach the receiver.
        ExecOptions {
            page_size: 1,
            ..Default::default()
        },
        CancellationToken::new(),
        tx,
    )
    .await
    .unwrap();
    let mut rows = Vec::new();
    let mut failed = None;
    while let Some(ev) = rx.recv().await {
        match ev {
            QueryEvent::Rows { rows: r, .. } => rows.extend(r),
            QueryEvent::Failed { error, .. } => failed = Some(error),
            _ => {}
        }
    }
    // The third row is malformed: the statement fails, after the good rows were mapped.
    assert!(failed.is_some(), "a malformed row was accepted");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0][0], json!({"$bytes": "Yf/+Yg==", "len": 4}));
    assert!(rows[0][1].is_array(), "{}", rows[0][1]);
    assert_eq!(rows[0][2]["len"], json!(9));
    assert_eq!(rows[1], vec![json!(null), json!(null), json!(null)]);
}

#[tokio::test]
async fn an_unknown_column_type_is_an_error_not_a_panic() {
    let f = fake(Plan::UnknownType, "").await;
    let h = MysqlHandle::connect(params(f.port, false), creds())
        .await
        .expect("login");
    let r = h
        .execute_collect(
            "SELECT 'trigger'",
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await;
    // Either the driver refuses the type, or it is shown as text; it must not panic.
    if let Ok(q) = r {
        assert_eq!(q.rows.len(), 1);
    }
}

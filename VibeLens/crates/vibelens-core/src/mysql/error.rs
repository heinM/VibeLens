//! Driver errors → [`CoreError`]. Messages never include credentials: `mysql_async` errors do
//! not carry the password and the options are never formatted. Server text is cut at 4 KiB.

use crate::error::CoreError;

/// Longest server message kept (R6).
const MAX_MESSAGE_BYTES: usize = 4096;

fn cut(s: &str) -> String {
    if s.len() <= MAX_MESSAGE_BYTES {
        return s.to_string();
    }
    let mut end = MAX_MESSAGE_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end])
}

/// Server error codes that mean "who you are was not accepted".
const AUTH_CODES: &[u16] = &[
    1044, // ER_DBACCESS_DENIED_ERROR
    1045, // ER_ACCESS_DENIED_ERROR
    1251, // ER_NOT_SUPPORTED_AUTH_MODE
    1698, // ER_ACCESS_DENIED_NO_PASSWORD_ERROR
    1820, // ER_MUST_CHANGE_PASSWORD
    1862, // ER_MUST_CHANGE_PASSWORD_LOGIN
    3118, // ER_ACCOUNT_HAS_BEEN_LOCKED
];

/// Maps a driver error to the closest [`CoreError`].
pub(super) fn map_error(e: &mysql_async::Error) -> CoreError {
    use mysql_async::{DriverError, Error};
    match e {
        Error::Server(s) => {
            let text = format!(
                "MySQL error {} (SQLSTATE {}): {}",
                s.code,
                s.state,
                cut(&s.message)
            );
            if AUTH_CODES.contains(&s.code) {
                CoreError::Auth(text)
            } else if s.code == 3024 {
                // ER_QUERY_TIMEOUT: max_execution_time
                CoreError::Timeout(text)
            } else {
                CoreError::Database(text)
            }
        }
        Error::Driver(DriverError::InsecureFullAuth | DriverError::CleartextPluginDisabled) => {
            CoreError::Auth(e.to_string())
        }
        Error::Driver(DriverError::UnknownAuthPlugin { name }) => CoreError::Auth(format!(
            "the server asked for the authentication plugin '{}', which VibeLens does not use",
            cut(name)
        )),
        Error::Driver(DriverError::LocalInfile(_)) => CoreError::PolicyDenied(
            "the server asked to read a file from this machine (LOCAL INFILE); VibeLens never \
             sends local files, and the connection was closed"
                .into(),
        ),
        Error::Driver(DriverError::ConnectionClosed) => {
            CoreError::NotConnected("the MySQL connection is closed".into())
        }
        Error::Driver(DriverError::NoClientSslFlagFromServer) => CoreError::Database(
            "TLS is on for this connection, but the server does not offer TLS".into(),
        ),
        Error::Driver(d) => CoreError::Database(format!("MySQL driver: {}", cut(&d.to_string()))),
        Error::Io(io) => CoreError::Database(format!("MySQL connection: {}", cut(&io.to_string()))),
        Error::Other(o) => CoreError::Database(cut(&o.to_string())),
        Error::Url(u) => CoreError::Invalid(u.to_string()),
    }
}

/// The connection is dead (not a statement error): retry before anything ran.
pub(super) fn is_connection_lost(e: &mysql_async::Error) -> bool {
    use mysql_async::{DriverError, Error};
    matches!(
        e,
        Error::Io(_) | Error::Driver(DriverError::ConnectionClosed)
    ) || matches!(e, Error::Server(s) if s.code == 4031)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_carry_code_and_sqlstate_and_are_cut() {
        let e = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1062,
            message: format!("Duplicate entry '{}' for key 'PRIMARY'", "x".repeat(10_000)),
            state: "23000".into(),
        });
        match map_error(&e) {
            CoreError::Database(m) => {
                assert!(m.starts_with("MySQL error 1062 (SQLSTATE 23000): Duplicate entry"));
                assert!(m.len() < 4200, "{}", m.len());
            }
            other => panic!("{other:?}"),
        }
        let e = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1045,
            message: "Access denied for user 'app'@'10.0.0.1' (using password: YES)".into(),
            state: "28000".into(),
        });
        assert!(matches!(map_error(&e), CoreError::Auth(_)));
        let e = mysql_async::Error::Server(mysql_async::ServerError {
            code: 3024,
            message: "Query execution was interrupted, maximum statement execution time exceeded"
                .into(),
            state: "HY000".into(),
        });
        assert!(matches!(map_error(&e), CoreError::Timeout(_)));
    }
}

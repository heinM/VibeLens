//! Secrets Manager: fetch and parse an RDS-style database secret.
//!
//! The secret value is held as `SecretString` and is never logged. Error messages name missing
//! *keys* only, never values.

use secrecy::SecretString;
use serde_json::Value;

use super::DbSecret;
use crate::error::{CoreError, Result};

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match obj.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Port as JSON number or numeric string (RDS-managed secrets use a number; hand-made ones often a string).
fn port_field(obj: &serde_json::Map<String, Value>) -> Result<Option<u16>> {
    let parsed = match obj.get("port") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
        Some(Value::String(s)) => s.trim().parse::<u16>().ok(),
        Some(_) => None,
    };
    match parsed {
        Some(p) if p != 0 => Ok(Some(p)),
        _ => Err(CoreError::Aws(
            "database secret has an invalid \"port\" (expected 1-65535)".into(),
        )),
    }
}

/// Parses the `SecretString` of an RDS database secret:
/// `{"username":…,"password":…,"host":…,"port":5432,"dbname":…,"engine":…}`. Extra fields are ignored.
pub fn parse_db_secret(secret_string: &str) -> Result<DbSecret> {
    let value: Value = serde_json::from_str(secret_string)
        .map_err(|_| CoreError::Aws("secret value is not JSON; expected an RDS-style database secret with username and password".into()))?;
    let Value::Object(obj) = value else {
        return Err(CoreError::Aws(
            "secret value is not a JSON object; expected an RDS-style database secret".into(),
        ));
    };
    let username = str_field(&obj, "username")
        .ok_or_else(|| CoreError::Aws("database secret has no \"username\" field".into()))?;
    // Passwords may legitimately have surrounding whitespace, so do not trim or reject on that.
    let password = match obj.get("password") {
        Some(Value::String(p)) if !p.is_empty() => p.clone(),
        _ => {
            return Err(CoreError::Aws(
                "database secret has no \"password\" field".into(),
            ));
        }
    };
    Ok(DbSecret {
        username,
        password: SecretString::from(password),
        host: str_field(&obj, "host"),
        port: port_field(&obj)?,
        dbname: str_field(&obj, "dbname").or_else(|| str_field(&obj, "dbName")),
        engine: str_field(&obj, "engine"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn rds_managed_shape_with_numeric_port() {
        let s = parse_db_secret(
            r#"{"username":"app","password":"p@ss w0rd ","engine":"postgres","host":"db.x.eu-west-1.rds.amazonaws.com","port":5432,"dbname":"appdb","dbInstanceIdentifier":"db-1"}"#,
        )
        .unwrap();
        assert_eq!(s.username, "app");
        assert_eq!(
            s.password.expose_secret(),
            "p@ss w0rd ",
            "password is not trimmed"
        );
        assert_eq!(s.host.as_deref(), Some("db.x.eu-west-1.rds.amazonaws.com"));
        assert_eq!(s.port, Some(5432));
        assert_eq!(s.dbname.as_deref(), Some("appdb"));
        assert_eq!(s.engine.as_deref(), Some("postgres"));
    }

    #[test]
    fn string_port_and_minimal_secret() {
        let s = parse_db_secret(r#"{"username":"u","password":"p","port":"6543"}"#).unwrap();
        assert_eq!(s.port, Some(6543));
        assert!(s.host.is_none() && s.dbname.is_none() && s.engine.is_none());
        let s =
            parse_db_secret(r#"{"username":"u","password":"p","host":"","port":"","dbname":null}"#)
                .unwrap();
        assert!(s.host.is_none() && s.port.is_none() && s.dbname.is_none());
    }

    #[test]
    fn invalid_ports_are_rejected() {
        for bad in [
            r#""port":0"#,
            r#""port":70000"#,
            r#""port":"abc""#,
            r#""port":true"#,
            r#""port":-1"#,
            r#""port":5432.5"#,
        ] {
            let json = format!(r#"{{"username":"u","password":"p",{bad}}}"#);
            let err = parse_db_secret(&json).unwrap_err();
            assert!(
                matches!(err, CoreError::Aws(ref m) if m.contains("port")),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn errors_are_clear_and_never_echo_values() {
        let e = parse_db_secret("hunter2-not-json").unwrap_err().to_string();
        assert!(e.contains("not JSON") && !e.contains("hunter2"));
        let e = parse_db_secret(r#"["hunter2"]"#).unwrap_err().to_string();
        assert!(e.contains("not a JSON object") && !e.contains("hunter2"));
        let e = parse_db_secret(r#"{"password":"hunter2"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("username") && !e.contains("hunter2"));
        let e = parse_db_secret(r#"{"username":"u"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("password"));
        let e = parse_db_secret(r#"{"username":"u","password":""}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("password"));
    }

    #[test]
    fn debug_of_db_secret_redacts_password() {
        let s = parse_db_secret(r#"{"username":"u","password":"hunter2"}"#).unwrap();
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}

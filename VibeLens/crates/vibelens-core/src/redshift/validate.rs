//! Checks on everything a Redshift profile or call puts into a signed Data API request.
//!
//! Nothing here is a secret, but these values are signed with the user's IAM identity and sent
//! to AWS, and the profile file can be edited by hand (R9). So each one is held to the character
//! set and length Redshift documents before any request is built, and the messages say which
//! rule was broken without echoing more than the field name.

use crate::error::{CoreError, Result};
use crate::model::{RedshiftAuth, RedshiftOptions, RedshiftTarget};

/// Redshift identifiers (database, schema, table and user names) are at most 127 bytes.
const MAX_NAME_BYTES: usize = 127;
/// Secrets Manager ARNs are at most 2048 characters.
const MAX_ARN_LEN: usize = 2048;

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(CoreError::Invalid(message.into()))
}

/// The whole profile section: target, database, region and authentication, including the rule
/// that a database user is only meaningful for a provisioned cluster. Called when a profile is
/// saved (through `cleartext::check_profile`) and again before it is used.
pub fn validate_options(o: &RedshiftOptions) -> Result<()> {
    match &o.target {
        RedshiftTarget::Provisioned { cluster_identifier } => {
            cluster_identifier_ok(cluster_identifier)?
        }
        RedshiftTarget::Serverless { workgroup_name } => workgroup_name_ok(workgroup_name)?,
    }
    database_name_ok(&o.database)?;
    region_ok(&o.aws.region)?;
    match &o.auth {
        RedshiftAuth::IamIdentity => {}
        RedshiftAuth::DbUser { db_user } => {
            if matches!(o.target, RedshiftTarget::Serverless { .. }) {
                return invalid(
                    "a database user can be chosen only for a provisioned cluster. Redshift \
                     Serverless maps the IAM identity to the database user; use IAM identity or a \
                     Secrets Manager secret instead",
                );
            }
            db_user_ok(db_user)?;
        }
        RedshiftAuth::Secret { secret_arn } => secret_arn_ok(secret_arn)?,
    }
    Ok(())
}

/// Cluster identifier, as AWS documents it and the connection form enforces: 1 to 63 lower-case
/// letters, digits or hyphens, starting with a letter, with no two hyphens in a row and no
/// hyphen at the end.
pub fn cluster_identifier_ok(s: &str) -> Result<()> {
    let s = s.trim();
    let ok = (1..=63).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.contains("--")
        && !s.ends_with('-');
    if ok {
        Ok(())
    } else {
        invalid(
            "the cluster identifier is not valid: it has 1 to 63 lower-case letters, digits or hyphens, \
             starts with a letter, and has no two hyphens in a row and no hyphen at the end",
        )
    }
}

/// Workgroup name: 3 to 64 lower-case letters, digits or hyphens (AWS's rule and the form's).
pub fn workgroup_name_ok(s: &str) -> Result<()> {
    let s = s.trim();
    let ok = (3..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        invalid(
            "the workgroup name is not valid: it has 3 to 64 lower-case letters, digits or hyphens",
        )
    }
}

/// A standard Redshift identifier: starts with a letter, an underscore or a multi-byte UTF-8
/// character; then letters, digits, underscores, dollar signs or multi-byte characters; at most
/// 127 bytes. Quoted identifiers with spaces or punctuation are not accepted here.
fn standard_identifier(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.len() <= MAX_NAME_BYTES
        && s.starts_with(|c: char| c.is_alphabetic() || c == '_' || !c.is_ascii())
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || !c.is_ascii())
        && !s.chars().any(char::is_control)
}

/// The profile's database: 1 to 64 characters, a lower-case letter or underscore and then
/// lower-case letters, digits, underscores or dollar signs. This is the connection form's rule
/// (and the documented one for a cluster's database name), so a profile the form saves is never
/// refused here and the other way round.
pub fn database_name_ok(s: &str) -> Result<()> {
    let s = s.trim();
    let ok = (1..=64).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'$');
    if ok {
        Ok(())
    } else {
        invalid(
            "the database name is not valid: it starts with a lower-case letter or underscore, \
             has only lower-case letters, digits, underscores or dollar signs, and is at most 64 \
             characters",
        )
    }
}

/// A database named for one call (the browser passes names `ListDatabases` returned): any
/// standard Redshift identifier, at most 127 bytes. Wider than [`database_name_ok`] because a
/// database created with SQL may have a longer or mixed-case name, which the server itself
/// reported.
pub fn requested_database_ok(s: &str) -> Result<()> {
    if standard_identifier(s) {
        Ok(())
    } else {
        invalid(
            "the database name is not valid: it starts with a letter or underscore, has only \
             letters, digits, underscores or dollar signs, and is at most 127 bytes",
        )
    }
}

/// Database user for `GetClusterCredentials`: starts with a letter, then letters, digits and
/// `_ + . @ -`; 1 to 64 characters (the API's documented limit and the form's). No `:` or `/`,
/// which the API refuses.
pub fn db_user_ok(s: &str) -> Result<()> {
    let s = s.trim();
    let ok = (1..=64).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'.' | b'@' | b'-'));
    if ok {
        Ok(())
    } else {
        invalid(
            "the database user is not valid: it starts with a letter and has only letters, \
             digits and _ + . @ -, at most 64 characters",
        )
    }
}

/// `arn:<partition>:secretsmanager:<region>:<12-digit account>:secret:<name>`. The name may hold
/// letters, digits and `/ _ + = . @ -` (Secrets Manager's own set, plus the random suffix).
pub fn secret_arn_ok(s: &str) -> Result<()> {
    let bad = || {
        invalid(
            "the secret is not a Secrets Manager ARN: expected \
             arn:aws:secretsmanager:<region>:<account id>:secret:<name>",
        )
    };
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_ARN_LEN {
        return bad();
    }
    let parts: Vec<&str> = s.splitn(7, ':').collect();
    let [arn, partition, service, region, account, kind, name] = parts.as_slice() else {
        return bad();
    };
    // `aws`, `aws-cn`, `aws-us-gov`, `aws-iso-b`, ...: `aws` and then lower-case letters or hyphens.
    let partition_ok = partition
        .strip_prefix("aws")
        .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
    let name_ok = !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'+' | b'=' | b'.' | b'@' | b'-')
        });
    if *arn == "arn"
        && partition_ok
        && *service == "secretsmanager"
        && region_ok(region).is_ok()
        && account.len() == 12
        && account.bytes().all(|b| b.is_ascii_digit())
        && *kind == "secret"
        && name_ok
    {
        Ok(())
    } else {
        bad()
    }
}

/// An AWS region code (`eu-west-1`, `us-gov-west-1`, `cn-north-1`). It becomes part of the
/// endpoint host name, so nothing else is accepted.
pub fn region_ok(s: &str) -> Result<()> {
    let s = s.trim();
    let ok = (1..=32).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.ends_with('-');
    if ok {
        Ok(())
    } else {
        invalid("the AWS region is not valid: expected a region code such as eu-west-1")
    }
}

/// A schema or table name that a browsing call sends to Redshift: non-empty, at most 127 bytes,
/// no control characters. Quoted identifiers may legitimately contain spaces and punctuation, so
/// this is a bound, not a grammar; the Data API sends it as a JSON string, never as SQL.
pub fn object_name_ok(what: &str, s: &str) -> Result<()> {
    if s.is_empty() || s.len() > MAX_NAME_BYTES || s.chars().any(char::is_control) {
        return invalid(format!(
            "the {what} name is not valid: it must be 1 to 127 bytes without control characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AwsIdentity, AwsTarget};

    fn opts(target: RedshiftTarget, auth: RedshiftAuth) -> RedshiftOptions {
        RedshiftOptions {
            target,
            database: "dev".into(),
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "eu-west-1".into(),
            },
            auth,
        }
    }
    fn prov() -> RedshiftTarget {
        RedshiftTarget::Provisioned {
            cluster_identifier: "analytics-1".into(),
        }
    }
    fn sls() -> RedshiftTarget {
        RedshiftTarget::Serverless {
            workgroup_name: "default-wg".into(),
        }
    }

    #[test]
    fn cluster_identifiers() {
        for ok in ["a", "analytics-1", "x1-y2-z3", &"a".repeat(63)] {
            cluster_identifier_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "1abc",
            "-abc",
            "abc-",
            "a--b",
            "a_b",
            "a.b",
            "a b",
            "a/b",
            "a?x=1",
            "Prod-Cluster",
            &"a".repeat(64),
        ] {
            assert!(cluster_identifier_ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn workgroup_names() {
        for ok in ["abc", "default-wg", "1wg", &"a".repeat(64)] {
            workgroup_name_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "ab",
            "a_bc",
            "WG-1",
            "wg.1",
            "wg 1",
            "wg/1",
            &"a".repeat(65),
        ] {
            assert!(workgroup_name_ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn database_names() {
        for ok in ["dev", "_x", "sales$2024", &"a".repeat(64)] {
            database_name_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            requested_database_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        // The profile field follows the form; a name the server listed may be wider.
        for listed in ["Sales", "Ünïcode", &"a".repeat(127)] {
            assert!(database_name_ok(listed).is_err(), "{listed}");
            requested_database_ok(listed).unwrap_or_else(|e| panic!("{listed}: {e}"));
        }
        for bad in [
            "",
            "1db",
            "my db",
            "db;drop",
            "db\"x",
            "d\nb",
            "db-1",
            &"a".repeat(128),
        ] {
            assert!(database_name_ok(bad).is_err(), "{bad:?}");
            assert!(requested_database_ok(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn db_users() {
        for ok in ["awsuser", "app.reader", "jane@example.com", "a-b_c+d"] {
            db_user_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "1user",
            "IAM:alice",
            "a/b",
            "a b",
            "a\u{0}",
            &"a".repeat(65),
        ] {
            assert!(db_user_ok(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn secret_arns() {
        for ok in [
            "arn:aws:secretsmanager:eu-west-1:123456789012:secret:redshift/app-AbCdEf",
            "arn:aws-us-gov:secretsmanager:us-gov-west-1:123456789012:secret:x",
            "arn:aws-cn:secretsmanager:cn-north-1:123456789012:secret:a_b+c=d.e@f-g",
        ] {
            secret_arn_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "my-secret",
            "arn:aws:s3:::bucket",
            "arn:aws:secretsmanager:eu-west-1:12345678901:secret:x",
            "arn:aws:secretsmanager:eu-west-1:123456789012:parameter:x",
            "arn:aws:secretsmanager:EU-WEST-1:123456789012:secret:x",
            "arn:aws:secretsmanager:eu-west-1:123456789012:secret:",
            "arn:aws:secretsmanager:eu-west-1:123456789012:secret:a b",
            "arn:aws:secretsmanager:eu-west-1:123456789012:secret:a:b",
            "arn:other:secretsmanager:eu-west-1:123456789012:secret:x",
            "arn:aws2:secretsmanager:eu-west-1:123456789012:secret:x",
            "https://secretsmanager.eu-west-1.amazonaws.com/x",
        ] {
            assert!(secret_arn_ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn regions() {
        for ok in ["eu-west-1", "us-gov-west-1", "cn-north-1", "ap-southeast-5"] {
            region_ok(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "EU-WEST-1",
            "eu-west-1.",
            "eu west 1",
            "evil.com/x",
            "-x",
        ] {
            assert!(region_ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn db_user_is_for_provisioned_clusters_only() {
        let db_user = RedshiftAuth::DbUser {
            db_user: "awsuser".into(),
        };
        validate_options(&opts(prov(), db_user.clone())).unwrap();
        let m = validate_options(&opts(sls(), db_user))
            .unwrap_err()
            .to_string();
        assert!(m.contains("provisioned"), "{m}");
        for auth in [
            RedshiftAuth::IamIdentity,
            RedshiftAuth::Secret {
                secret_arn: "arn:aws:secretsmanager:eu-west-1:123456789012:secret:x".into(),
            },
        ] {
            validate_options(&opts(prov(), auth.clone())).unwrap();
            validate_options(&opts(sls(), auth)).unwrap();
        }
    }

    #[test]
    fn every_field_is_checked() {
        let mut o = opts(prov(), RedshiftAuth::IamIdentity);
        o.database = "bad name".into();
        assert!(validate_options(&o).is_err());
        let mut o = opts(prov(), RedshiftAuth::IamIdentity);
        o.aws.region = "x/y".into();
        assert!(validate_options(&o).is_err());
        let o = opts(
            RedshiftTarget::Provisioned {
                cluster_identifier: "a--b".into(),
            },
            RedshiftAuth::IamIdentity,
        );
        assert!(validate_options(&o).is_err());
        let o = opts(
            prov(),
            RedshiftAuth::Secret {
                secret_arn: "not-an-arn".into(),
            },
        );
        assert!(validate_options(&o).is_err());
    }

    #[test]
    fn object_names_are_bounded() {
        object_name_ok("schema", "public").unwrap();
        object_name_ok("table", "My Table").unwrap();
        assert!(object_name_ok("schema", "").is_err());
        assert!(object_name_ok("schema", "a\nb").is_err());
        assert!(object_name_ok("table", &"a".repeat(128)).is_err());
    }

    #[test]
    fn options_reject_malformed_enums_instead_of_defaulting() {
        let good = serde_json::json!({
            "target": {"kind": "serverless", "workgroupName": "wg-one"},
            "database": "dev",
            "aws": {"identity": {"kind": "defaultChain"}, "region": "eu-west-1"},
            "auth": {"kind": "iamIdentity"}
        });
        let o: RedshiftOptions = serde_json::from_value(good.clone()).unwrap();
        assert_eq!(o.auth, RedshiftAuth::IamIdentity);
        let mut bad = good.clone();
        bad["auth"] = serde_json::json!({"kind": "password"});
        assert!(serde_json::from_value::<RedshiftOptions>(bad).is_err());
        let mut bad = good.clone();
        bad["target"] = serde_json::json!({"kind": "cluster", "clusterIdentifier": "x"});
        assert!(serde_json::from_value::<RedshiftOptions>(bad).is_err());
        let mut bad = good;
        bad.as_object_mut().unwrap().remove("auth");
        assert!(serde_json::from_value::<RedshiftOptions>(bad).is_err());
    }
}

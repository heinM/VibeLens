//! JDBC URLs as DataGrip and DBeaver store them:
//! `jdbc:postgresql://host[:port][,host2[:port2]]/db?k=v&...`, `jdbc:postgresql:db`,
//! `jdbc:redis://[user[:pw]@]host:port/db`, `jdbc:redshift://...`, and friends.

use std::collections::HashMap;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct JdbcTarget {
    /// The sub-protocol: `postgresql`, `redis`, `redshift`, `mysql`, ...
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub user: Option<String>,
    /// Query parameters (lower-cased keys). `password` is dropped on purpose.
    pub params: HashMap<String, String>,
    /// More than one host was listed (failover); only the first is used.
    pub extra_hosts: bool,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `host[:port]`, including `[ipv6]:port`.
fn host_port(s: &str) -> (String, Option<u16>) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[')
        && let Some((h, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return (h.to_string(), port);
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()),
        _ => (s.to_string(), None),
    }
}

pub(super) fn parse(url: &str) -> Option<JdbcTarget> {
    let rest = url.trim().strip_prefix("jdbc:")?;
    let (scheme, after) = rest.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    let (body, query) = match after.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (after, None),
    };
    let mut t = JdbcTarget {
        scheme,
        ..Default::default()
    };
    if let Some(q) = query {
        for kv in q.split(['&', ';']) {
            if let Some((k, v)) = kv.split_once('=') {
                let k = k.trim().to_ascii_lowercase();
                if k != "password" && !k.is_empty() {
                    t.params.insert(k, percent_decode(v.trim()));
                }
            }
        }
    }
    if let Some(net) = body.strip_prefix("//") {
        let (authority, path) = match net.find('/') {
            Some(i) => (&net[..i], Some(&net[i + 1..])),
            None => (net, None),
        };
        let authority = match authority.rsplit_once('@') {
            Some((userinfo, hosts)) => {
                let user = userinfo.split(':').next().unwrap_or("");
                if !user.is_empty() {
                    t.user = Some(percent_decode(user));
                }
                hosts
            }
            None => authority,
        };
        let mut hosts = authority.split(',');
        let (h, p) = host_port(hosts.next().unwrap_or(""));
        t.host = h;
        t.port = p;
        t.extra_hosts = hosts.next().is_some();
        if let Some(p) = path {
            let db = percent_decode(p.split(';').next().unwrap_or("").trim());
            if !db.is_empty() {
                t.database = Some(db);
            }
        }
    } else {
        // `jdbc:postgresql:db` (localhost) or `jdbc:postgresql:` (all defaults).
        let db = body.trim();
        t.host = "localhost".into();
        if !db.is_empty() {
            t.database = Some(percent_decode(db));
        }
    }
    if t.user.is_none() {
        t.user = t.params.get("user").cloned().filter(|u| !u.is_empty());
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_urls() {
        let t = parse(
            "jdbc:postgresql://db.example.com:6432/orders?sslmode=require&user=app&password=nope",
        )
        .unwrap();
        assert_eq!(t.scheme, "postgresql");
        assert_eq!((t.host.as_str(), t.port), ("db.example.com", Some(6432)));
        assert_eq!(t.database.as_deref(), Some("orders"));
        assert_eq!(t.user.as_deref(), Some("app"));
        assert_eq!(t.params.get("sslmode").map(String::as_str), Some("require"));
        assert!(
            !t.params.contains_key("password"),
            "passwords are never kept"
        );

        let t = parse("jdbc:postgresql://localhost/postgres").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("localhost", None));
        let t = parse("jdbc:postgresql://[::1]:5433/x").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", Some(5433)));
        let t = parse("jdbc:postgresql://a:1,b:2/x").unwrap();
        assert_eq!(t.host, "a");
        assert!(t.extra_hosts);
        let t = parse("jdbc:postgresql:mydb").unwrap();
        assert_eq!(
            (t.host.as_str(), t.database.as_deref()),
            ("localhost", Some("mydb"))
        );
        let t = parse("jdbc:postgresql://h/my%20db").unwrap();
        assert_eq!(t.database.as_deref(), Some("my db"));
        assert!(parse("postgresql://h/db").is_none());
    }

    #[test]
    fn redis_urls() {
        let t = parse("jdbc:redis://cache.internal:6380/2").unwrap();
        assert_eq!(t.scheme, "redis");
        assert_eq!((t.host.as_str(), t.port), ("cache.internal", Some(6380)));
        let t = parse("jdbc:redis://alice:secret@h:6379/0").unwrap();
        assert_eq!(t.user.as_deref(), Some("alice"));
    }
}

//! Binary-protocol value → JSON [`Cell`] encoding.
//!
//! Rules (see `model::query::Cell`): `int2/int4/oid/float4/float8` become JSON numbers
//! (non-finite floats become strings); `int8`, `numeric` and `money` become **strings** so JS
//! never rounds them; `bytea` becomes `{"$bytes": base64, "len": n}`; `json/jsonb` are embedded;
//! temporal types are strings (RFC3339 for `timestamptz`); arrays are (nested) JSON arrays;
//! anything we cannot decode becomes a readable `"<typename>"` placeholder. Nothing in this
//! module panics on malformed input.

use std::error::Error;
use std::net::{Ipv4Addr, Ipv6Addr};

use base64::Engine as _;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::{Map, Value};
use tokio_postgres::types::{FromSql, Kind, Type};

use crate::model::Cell;

/// Microseconds between the Unix epoch and the Postgres epoch (2000-01-01).
const PG_EPOCH_UNIX_MICROS: i64 = 946_684_800_000_000;
/// `chrono` day number (days from 0001-01-01 CE, 1 = that day) of 2000-01-01.
const PG_EPOCH_DAYS_FROM_CE: i32 = 730_120;
const MICROS_PER_SEC: i64 = 1_000_000;

/// `FromSql` wrapper that accepts any column type and never fails.
#[derive(Debug, Clone, PartialEq)]
pub struct CellValue(pub Cell);

impl<'a> FromSql<'a> for CellValue {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(CellValue(decode(ty, raw)))
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(CellValue(Value::Null))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Decodes one non-NULL binary value. Falls back to `"<typename>"` when the value is malformed
/// or the type is not supported.
pub fn decode(ty: &Type, raw: &[u8]) -> Cell {
    decode_inner(ty, raw).unwrap_or_else(|| Value::String(format!("<{}>", ty.name())))
}

fn decode_inner(ty: &Type, raw: &[u8]) -> Option<Cell> {
    match ty.kind() {
        Kind::Array(inner) => return decode_array(inner, raw),
        Kind::Domain(base) => return decode_inner(base, raw),
        Kind::Enum(_) => return utf8(raw),
        Kind::Range(inner) => return decode_range(inner, raw),
        _ => {}
    }
    match *ty {
        Type::BOOL => match raw {
            [b] => Some(Value::Bool(*b != 0)),
            _ => None,
        },
        Type::INT2 => Some(Value::from(i16::from_be_bytes(raw.try_into().ok()?))),
        Type::INT4 => Some(Value::from(i32::from_be_bytes(raw.try_into().ok()?))),
        Type::OID | Type::XID | Type::CID => {
            Some(Value::from(u32::from_be_bytes(raw.try_into().ok()?)))
        }
        Type::INT8 => Some(Value::String(
            i64::from_be_bytes(raw.try_into().ok()?).to_string(),
        )),
        Type::FLOAT4 => Some(float32(f32::from_be_bytes(raw.try_into().ok()?))),
        Type::FLOAT8 => Some(float64(f64::from_be_bytes(raw.try_into().ok()?))),
        Type::NUMERIC => numeric_to_string(raw).map(Value::String),
        Type::MONEY => Some(Value::String(money_to_string(i64::from_be_bytes(
            raw.try_into().ok()?,
        )))),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN | Type::XML => {
            utf8(raw)
        }
        Type::CHAR => match raw {
            [b] => Some(Value::String((*b as char).to_string())),
            _ => None,
        },
        Type::UUID => uuid_to_string(raw).map(Value::String),
        Type::JSON => json_text(raw),
        Type::JSONB => match raw.split_first() {
            Some((1, rest)) => json_text(rest),
            _ => None,
        },
        Type::JSONPATH => match raw.split_first() {
            Some((1, rest)) => utf8(rest),
            _ => None,
        },
        Type::BYTEA => Some(bytes_cell(raw)),
        Type::TIMESTAMP => timestamp(raw, false),
        Type::TIMESTAMPTZ => timestamp(raw, true),
        Type::DATE => date(raw),
        Type::TIME => time_of_day(i64::from_be_bytes(raw.try_into().ok()?)).map(Value::String),
        Type::TIMETZ => timetz(raw),
        Type::INTERVAL => interval(raw),
        Type::INET => inet(raw, false),
        Type::CIDR => inet(raw, true),
        Type::MACADDR | Type::MACADDR8 => mac(raw),
        Type::BIT | Type::VARBIT => bit_string(raw),
        Type::POINT => point(raw),
        Type::PG_LSN => {
            let v = u64::from_be_bytes(raw.try_into().ok()?);
            Some(Value::String(format!(
                "{:X}/{:X}",
                v >> 32,
                v & 0xFFFF_FFFF
            )))
        }
        Type::INT2_VECTOR => decode_array(&Type::INT2, raw),
        Type::OID_VECTOR => decode_array(&Type::OID, raw),
        _ => by_name(ty, raw),
    }
}

/// Extension types (dynamic OIDs) and the `reg*` family.
fn by_name(ty: &Type, raw: &[u8]) -> Option<Cell> {
    match ty.name() {
        "citext" | "ltree" | "lquery" | "ltxtquery" => utf8(raw),
        n if n.starts_with("reg") && raw.len() == 4 => {
            Some(Value::from(u32::from_be_bytes(raw.try_into().ok()?)))
        }
        _ => None,
    }
}

fn utf8(raw: &[u8]) -> Option<Cell> {
    std::str::from_utf8(raw)
        .ok()
        .map(|s| Value::String(s.to_owned()))
}

fn json_text(raw: &[u8]) -> Option<Cell> {
    match serde_json::from_slice::<Value>(raw) {
        Ok(v) => Some(v),
        // Not valid for serde_json (e.g. nesting too deep): keep the text rather than lose it.
        Err(_) => utf8(raw),
    }
}

fn float32(v: f32) -> Cell {
    if v.is_finite() {
        // Go through the shortest decimal repr so 0.1f32 shows as 0.1, not 0.10000000149011612.
        let widened = v.to_string().parse::<f64>().unwrap_or(f64::from(v));
        serde_json::Number::from_f64(widened)
            .map_or_else(|| Value::String(v.to_string()), Value::Number)
    } else {
        Value::String(non_finite(f64::from(v)))
    }
}

fn float64(v: f64) -> Cell {
    if v.is_finite() {
        serde_json::Number::from_f64(v).map_or_else(|| Value::String(v.to_string()), Value::Number)
    } else {
        Value::String(non_finite(v))
    }
}

fn non_finite(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_sign_positive() {
        "Infinity".into()
    } else {
        "-Infinity".into()
    }
}

/// `money` is an int8 count of the currency's minor unit. We assume two fractional digits
/// (the default for most `lc_monetary` locales) and emit a plain decimal without symbol.
fn money_to_string(cents: i64) -> String {
    let neg = cents < 0;
    let abs = cents.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if neg { "-" } else { "" },
        abs / 100,
        abs % 100
    )
}

fn uuid_to_string(raw: &[u8]) -> Option<String> {
    if raw.len() != 16 {
        return None;
    }
    let h = hex::encode(raw);
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

fn bytes_cell(raw: &[u8]) -> Cell {
    let mut m = Map::new();
    m.insert(
        "$bytes".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(raw)),
    );
    m.insert("len".into(), Value::from(raw.len()));
    Value::Object(m)
}

// ---- numeric -------------------------------------------------------------------------------

const NUMERIC_POS: u16 = 0x0000;
const NUMERIC_NEG: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_PINF: u16 = 0xD000;
const NUMERIC_NINF: u16 = 0xF000;

/// Decodes the binary `numeric` wire format into an exact decimal string.
///
/// Wire layout (big endian): `ndigits:i16, weight:i16, sign:u16, dscale:i16, digits:[i16; ndigits]`
/// where each digit is a base-10000 group and `weight` is the position of the first group
/// (0 = units group, 1 = ten-thousands, -1 = first fractional group, ...). `dscale` is the number of
/// decimal digits to show after the point. Handles `NaN` and `±Infinity`.
pub fn numeric_to_string(raw: &[u8]) -> Option<String> {
    let mut r = Reader::new(raw);
    let ndigits = usize::try_from(r.i16()?).ok()?;
    let weight = i32::from(r.i16()?);
    let sign = r.u16()?;
    let dscale = usize::try_from(r.i16()?).ok()?;
    match sign {
        NUMERIC_NAN => return Some("NaN".into()),
        NUMERIC_PINF => return Some("Infinity".into()),
        NUMERIC_NINF => return Some("-Infinity".into()),
        NUMERIC_POS | NUMERIC_NEG => {}
        _ => return None,
    }
    if r.remaining() < ndigits * 2 {
        return None;
    }
    let mut digits = Vec::with_capacity(ndigits);
    for _ in 0..ndigits {
        let d = r.i16()?;
        if !(0..10_000).contains(&d) {
            return None;
        }
        digits.push(d as u16);
    }
    // Group at "weight position" w (units group = 0).
    let group = |w: i32| -> u16 {
        let idx = weight - w;
        if idx < 0 {
            0
        } else {
            digits.get(idx as usize).copied().unwrap_or(0)
        }
    };

    let mut out = String::new();
    if sign == NUMERIC_NEG && ndigits > 0 {
        out.push('-');
    }
    // Integer part.
    if weight < 0 {
        out.push('0');
    } else {
        // Guard against absurd weights from a corrupt packet (real max is 131072 digits).
        if weight > 40_000 {
            return None;
        }
        for w in (0..=weight).rev() {
            let g = group(w);
            if w == weight {
                out.push_str(&g.to_string());
            } else {
                out.push_str(&format!("{g:04}"));
            }
        }
    }
    // Fractional part, padded/truncated to dscale.
    if dscale > 0 {
        out.push('.');
        let mut frac = String::with_capacity(dscale + 4);
        let mut w = -1;
        while frac.len() < dscale {
            frac.push_str(&format!("{:04}", group(w)));
            w -= 1;
        }
        frac.truncate(dscale);
        out.push_str(&frac);
    }
    Some(out)
}

// ---- temporal ------------------------------------------------------------------------------

fn timestamp(raw: &[u8], with_tz: bool) -> Option<Cell> {
    let v = i64::from_be_bytes(raw.try_into().ok()?);
    if v == i64::MAX {
        return Some(Value::String("infinity".into()));
    }
    if v == i64::MIN {
        return Some(Value::String("-infinity".into()));
    }
    let dt = DateTime::<Utc>::from_timestamp_micros(v.checked_add(PG_EPOCH_UNIX_MICROS)?)?;
    Some(Value::String(if with_tz {
        dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    } else {
        dt.naive_utc().format("%Y-%m-%dT%H:%M:%S%.f").to_string()
    }))
}

fn date(raw: &[u8]) -> Option<Cell> {
    let v = i32::from_be_bytes(raw.try_into().ok()?);
    if v == i32::MAX {
        return Some(Value::String("infinity".into()));
    }
    if v == i32::MIN {
        return Some(Value::String("-infinity".into()));
    }
    let d = NaiveDate::from_num_days_from_ce_opt(v.checked_add(PG_EPOCH_DAYS_FROM_CE)?)?;
    Some(Value::String(d.format("%Y-%m-%d").to_string()))
}

/// `HH:MM:SS[.ffffff]` from microseconds since midnight (Postgres allows `24:00:00`).
fn time_of_day(micros: i64) -> Option<String> {
    if !(0..=86_400 * MICROS_PER_SEC).contains(&micros) {
        return None;
    }
    let secs = micros / MICROS_PER_SEC;
    let frac = micros % MICROS_PER_SEC;
    let mut s = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    push_fraction(&mut s, frac);
    Some(s)
}

fn push_fraction(s: &mut String, micros_frac: i64) {
    if micros_frac != 0 {
        let f = format!("{micros_frac:06}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
}

fn timetz(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 12 {
        return None;
    }
    let micros = i64::from_be_bytes(raw[0..8].try_into().ok()?);
    // Postgres stores the zone as seconds *west* of UTC.
    let west = i32::from_be_bytes(raw[8..12].try_into().ok()?);
    let east = -i64::from(west);
    let mut s = time_of_day(micros)?;
    s.push(if east < 0 { '-' } else { '+' });
    let a = east.abs();
    s.push_str(&format!("{:02}:{:02}", a / 3600, (a / 60) % 60));
    if a % 60 != 0 {
        s.push_str(&format!(":{:02}", a % 60));
    }
    Some(Value::String(s))
}

fn interval(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 16 {
        return None;
    }
    let micros = i64::from_be_bytes(raw[0..8].try_into().ok()?);
    let days = i32::from_be_bytes(raw[8..12].try_into().ok()?);
    let months = i32::from_be_bytes(raw[12..16].try_into().ok()?);
    Some(Value::String(interval_to_string(micros, days, months)))
}

/// Formats an interval like Postgres' default `IntervalStyle = postgres`
/// (`1 year 2 mons 3 days 04:05:06.789`).
pub fn interval_to_string(micros: i64, days: i32, months: i32) -> String {
    let years = months / 12;
    let mons = months % 12;
    let mut parts: Vec<String> = Vec::new();
    let mut is_before = false;
    let mut push = |value: i64, text: String| {
        if value == 0 {
            return;
        }
        let sign = if is_before && value > 0 { "+" } else { "" };
        parts.push(format!("{sign}{text}"));
        if value < 0 {
            is_before = true;
        }
    };
    push(
        i64::from(years),
        format!("{} year{}", years, if years == 1 { "" } else { "s" }),
    );
    push(
        i64::from(mons),
        format!("{} mon{}", mons, if mons == 1 { "" } else { "s" }),
    );
    push(
        i64::from(days),
        format!("{} day{}", days, if days == 1 { "" } else { "s" }),
    );
    if micros != 0 {
        let neg = micros < 0;
        let abs = micros.unsigned_abs();
        let total_secs = abs / MICROS_PER_SEC as u64;
        let frac = (abs % MICROS_PER_SEC as u64) as i64;
        let mut t = format!(
            "{:02}:{:02}:{:02}",
            total_secs / 3600,
            (total_secs / 60) % 60,
            total_secs % 60
        );
        push_fraction(&mut t, frac);
        push(
            if neg { -1 } else { 1 },
            format!("{}{}", if neg { "-" } else { "" }, t),
        );
    }
    if parts.is_empty() {
        "00:00:00".into()
    } else {
        parts.join(" ")
    }
}

// ---- network / misc ------------------------------------------------------------------------

fn inet(raw: &[u8], is_cidr: bool) -> Option<Cell> {
    // family (2 = IPv4, 3 = IPv6), bits, is_cidr, nb, address bytes
    let (&family, rest) = raw.split_first()?;
    let (&bits, rest) = rest.split_first()?;
    let (_, rest) = rest.split_first()?;
    let (&nb, addr) = rest.split_first()?;
    if addr.len() != usize::from(nb) {
        return None;
    }
    let (text, max_bits) = match (family, addr.len()) {
        (2, 4) => (
            Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
            32u8,
        ),
        (3, 16) => {
            let octets: [u8; 16] = addr.try_into().ok()?;
            (Ipv6Addr::from(octets).to_string(), 128u8)
        }
        _ => return None,
    };
    if bits > max_bits {
        return None;
    }
    Some(Value::String(if is_cidr || bits != max_bits {
        format!("{text}/{bits}")
    } else {
        text
    }))
}

fn mac(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 6 && raw.len() != 8 {
        return None;
    }
    Some(Value::String(
        raw.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    ))
}

fn bit_string(raw: &[u8]) -> Option<Cell> {
    let nbits = usize::try_from(i32::from_be_bytes(raw.get(0..4)?.try_into().ok()?)).ok()?;
    let data = raw.get(4..)?;
    if data.len() < nbits.div_ceil(8) {
        return None;
    }
    Some(Value::String(
        (0..nbits)
            .map(|i| {
                if data[i / 8] & (0x80 >> (i % 8)) != 0 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect(),
    ))
}

fn point(raw: &[u8]) -> Option<Cell> {
    if raw.len() != 16 {
        return None;
    }
    let x = f64::from_be_bytes(raw[0..8].try_into().ok()?);
    let y = f64::from_be_bytes(raw[8..16].try_into().ok()?);
    Some(Value::String(format!("({x},{y})")))
}

// ---- arrays and ranges ---------------------------------------------------------------------

fn decode_array(elem: &Type, raw: &[u8]) -> Option<Cell> {
    let mut r = Reader::new(raw);
    let ndim = r.i32()?;
    let _flags = r.i32()?;
    let _elem_oid = r.u32()?;
    if ndim == 0 {
        return Some(Value::Array(Vec::new()));
    }
    if !(1..=6).contains(&ndim) {
        return None;
    }
    let mut dims = Vec::with_capacity(ndim as usize);
    for _ in 0..ndim {
        let len = usize::try_from(r.i32()?).ok()?;
        let _lower_bound = r.i32()?;
        dims.push(len);
    }
    let total = dims.iter().try_fold(1usize, |a, &d| a.checked_mul(d))?;
    // Every element costs at least its 4-byte length header; reject impossible sizes up front.
    if total > r.remaining() / 4 {
        return None;
    }
    build_array(&dims, &mut r, elem)
}

fn build_array(dims: &[usize], r: &mut Reader<'_>, elem: &Type) -> Option<Cell> {
    let (&len, rest) = dims.split_first()?;
    let mut out = Vec::with_capacity(len.min(r.remaining() / 4));
    for _ in 0..len {
        if rest.is_empty() {
            let n = r.i32()?;
            if n == -1 {
                out.push(Value::Null);
            } else {
                let bytes = r.take(usize::try_from(n).ok()?)?;
                out.push(decode(elem, bytes));
            }
        } else {
            out.push(build_array(rest, r, elem)?);
        }
    }
    Some(Value::Array(out))
}

/// Ranges become their Postgres text form, e.g. `[1,10)`.
fn decode_range(elem: &Type, raw: &[u8]) -> Option<Cell> {
    let mut r = Reader::new(raw);
    let flags = r.u8()?;
    if flags & 0x01 != 0 {
        return Some(Value::String("empty".into()));
    }
    let (lb_inc, ub_inc, lb_inf, ub_inf) = (
        flags & 0x02 != 0,
        flags & 0x04 != 0,
        flags & 0x08 != 0,
        flags & 0x10 != 0,
    );
    let mut bound = |infinite: bool| -> Option<String> {
        if infinite {
            return Some(String::new());
        }
        let n = usize::try_from(r.i32()?).ok()?;
        let bytes = r.take(n)?;
        Some(match decode(elem, bytes) {
            Value::String(s) => s,
            other => other.to_string(),
        })
    };
    let lower = bound(lb_inf)?;
    let upper = bound(ub_inf)?;
    let open = if lb_inc && !lb_inf { '[' } else { '(' };
    let close = if ub_inc && !ub_inf { ']' } else { ')' };
    Some(Value::String(format!("{open}{lower},{upper}{close}")))
}

// ---- reader --------------------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn numeric_raw(weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(digits.len() as i16).to_be_bytes());
        v.extend_from_slice(&weight.to_be_bytes());
        v.extend_from_slice(&sign.to_be_bytes());
        v.extend_from_slice(&dscale.to_be_bytes());
        for d in digits {
            v.extend_from_slice(&d.to_be_bytes());
        }
        v
    }

    fn num(weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> String {
        numeric_to_string(&numeric_raw(weight, sign, dscale, digits)).expect("valid numeric")
    }

    #[test]
    fn numeric_zero_and_scale() {
        assert_eq!(num(0, NUMERIC_POS, 0, &[]), "0");
        assert_eq!(num(0, NUMERIC_POS, 2, &[]), "0.00");
        assert_eq!(num(0, NUMERIC_POS, 3, &[0]), "0.000");
    }

    #[test]
    fn numeric_integers() {
        assert_eq!(num(0, NUMERIC_POS, 0, &[1]), "1");
        assert_eq!(num(0, NUMERIC_NEG, 0, &[42]), "-42");
        assert_eq!(num(1, NUMERIC_POS, 0, &[1, 2345]), "12345");
        // 100000000 is stored as a single group with weight 2: trailing zero groups are implied.
        assert_eq!(num(2, NUMERIC_POS, 0, &[1]), "100000000");
        assert_eq!(num(1, NUMERIC_POS, 0, &[10, 0]), "100000");
        assert_eq!(num(2, NUMERIC_POS, 0, &[1, 0, 0]), "100000000");
        assert_eq!(num(1, NUMERIC_NEG, 0, &[1, 0]), "-10000");
    }

    #[test]
    fn numeric_fractions() {
        assert_eq!(num(0, NUMERIC_POS, 2, &[3, 1400]), "3.14");
        assert_eq!(num(0, NUMERIC_NEG, 4, &[3, 1415]), "-3.1415");
        // 0.0001 -> weight -1, one group 1
        assert_eq!(num(-1, NUMERIC_POS, 4, &[1]), "0.0001");
        // 0.00001 -> weight -2, group 1000
        assert_eq!(num(-2, NUMERIC_POS, 5, &[1000]), "0.00001");
        // 0.5 with extra display scale
        assert_eq!(num(-1, NUMERIC_POS, 6, &[5000]), "0.500000");
        // dscale larger than stored digits pads with zeros
        assert_eq!(num(0, NUMERIC_POS, 6, &[7]), "7.000000");
        // integer with fractional trailing zeros: 12.50
        assert_eq!(num(0, NUMERIC_POS, 2, &[12, 5000]), "12.50");
    }

    #[test]
    fn numeric_large_value_is_exact() {
        // 12345678901234567890.123456789
        let s = num(
            4,
            NUMERIC_POS,
            9,
            &[1234, 5678, 9012, 3456, 7890, 1234, 5678, 9000],
        );
        assert_eq!(s, "12345678901234567890.123456789");
        // Leading group is not zero padded: 1234567.5
        assert_eq!(num(1, NUMERIC_POS, 1, &[123, 4567, 5000]), "1234567.5");
    }

    #[test]
    fn numeric_special_values() {
        assert_eq!(num(0, NUMERIC_NAN, 0, &[]), "NaN");
        assert_eq!(num(0, NUMERIC_PINF, 0, &[]), "Infinity");
        assert_eq!(num(0, NUMERIC_NINF, 0, &[]), "-Infinity");
    }

    #[test]
    fn numeric_rejects_malformed() {
        assert!(numeric_to_string(&[]).is_none());
        assert!(
            numeric_to_string(&[0, 5, 0, 0, 0, 0, 0, 0]).is_none(),
            "claims 5 digits, has none"
        );
        assert!(
            numeric_to_string(&numeric_raw(0, 0x1234, 0, &[1])).is_none(),
            "bad sign"
        );
        assert!(
            numeric_to_string(&numeric_raw(0, NUMERIC_POS, 0, &[10_000])).is_none(),
            "digit out of range"
        );
    }

    #[test]
    fn int8_is_a_string_beyond_2_pow_53() {
        let v = i64::MAX;
        assert_eq!(
            decode(&Type::INT8, &v.to_be_bytes()),
            json!("9223372036854775807")
        );
        let v: i64 = (1 << 53) + 1;
        assert_eq!(
            decode(&Type::INT8, &v.to_be_bytes()),
            json!("9007199254740993")
        );
        assert_eq!(
            decode(&Type::INT8, &i64::MIN.to_be_bytes()),
            json!("-9223372036854775808")
        );
    }

    #[test]
    fn small_ints_and_bools() {
        assert_eq!(decode(&Type::INT2, &(-5i16).to_be_bytes()), json!(-5));
        assert_eq!(
            decode(&Type::INT4, &i32::MAX.to_be_bytes()),
            json!(2147483647)
        );
        assert_eq!(
            decode(&Type::OID, &u32::MAX.to_be_bytes()),
            json!(4294967295u32)
        );
        assert_eq!(decode(&Type::BOOL, &[1]), json!(true));
        assert_eq!(decode(&Type::BOOL, &[0]), json!(false));
    }

    #[test]
    fn floats() {
        assert_eq!(decode(&Type::FLOAT8, &1.5f64.to_be_bytes()), json!(1.5));
        assert_eq!(decode(&Type::FLOAT4, &0.1f32.to_be_bytes()), json!(0.1));
        assert_eq!(decode(&Type::FLOAT8, &f64::NAN.to_be_bytes()), json!("NaN"));
        assert_eq!(
            decode(&Type::FLOAT8, &f64::INFINITY.to_be_bytes()),
            json!("Infinity")
        );
        assert_eq!(
            decode(&Type::FLOAT4, &f32::NEG_INFINITY.to_be_bytes()),
            json!("-Infinity")
        );
    }

    #[test]
    fn money_formats_two_decimals() {
        assert_eq!(
            decode(&Type::MONEY, &123456i64.to_be_bytes()),
            json!("1234.56")
        );
        assert_eq!(decode(&Type::MONEY, &(-5i64).to_be_bytes()), json!("-0.05"));
        assert_eq!(
            decode(&Type::MONEY, &i64::MIN.to_be_bytes()),
            json!("-92233720368547758.08")
        );
    }

    #[test]
    fn text_uuid_bytea_json() {
        assert_eq!(decode(&Type::TEXT, b"h\xc3\xa9llo"), json!("héllo"));
        let u: Vec<u8> = (0u8..16).collect();
        assert_eq!(
            decode(&Type::UUID, &u),
            json!("00010203-0405-0607-0809-0a0b0c0d0e0f")
        );
        assert_eq!(
            decode(&Type::BYTEA, &[0xde, 0xad, 0xbe, 0xef]),
            json!({"$bytes": "3q2+7w==", "len": 4})
        );
        assert_eq!(decode(&Type::JSON, br#"{"a":[1,2]}"#), json!({"a": [1, 2]}));
        let mut jb = vec![1u8];
        jb.extend_from_slice(br#"{"b":null}"#);
        assert_eq!(decode(&Type::JSONB, &jb), json!({"b": null}));
    }

    #[test]
    fn invalid_utf8_falls_back_to_type_name() {
        assert_eq!(decode(&Type::TEXT, &[0xff, 0xfe]), json!("<text>"));
        assert_eq!(decode(&Type::INT4, &[1, 2]), json!("<int4>"));
    }

    #[test]
    fn timestamps_and_dates() {
        // 2000-01-01T00:00:00 is micros 0.
        assert_eq!(
            decode(&Type::TIMESTAMP, &0i64.to_be_bytes()),
            json!("2000-01-01T00:00:00")
        );
        assert_eq!(
            decode(&Type::TIMESTAMPTZ, &0i64.to_be_bytes()),
            json!("2000-01-01T00:00:00Z")
        );
        let us = 86_400 * MICROS_PER_SEC + 1_500_000;
        assert_eq!(
            decode(&Type::TIMESTAMPTZ, &us.to_be_bytes()),
            json!("2000-01-02T00:00:01.500Z")
        );
        assert_eq!(
            decode(&Type::TIMESTAMP, &us.to_be_bytes()),
            json!("2000-01-02T00:00:01.500")
        );
        assert_eq!(
            decode(&Type::TIMESTAMPTZ, &i64::MAX.to_be_bytes()),
            json!("infinity")
        );
        assert_eq!(
            decode(&Type::TIMESTAMP, &i64::MIN.to_be_bytes()),
            json!("-infinity")
        );
        assert_eq!(
            decode(&Type::DATE, &0i32.to_be_bytes()),
            json!("2000-01-01")
        );
        assert_eq!(
            decode(&Type::DATE, &(-1i32).to_be_bytes()),
            json!("1999-12-31")
        );
        assert_eq!(
            decode(&Type::DATE, &8766i32.to_be_bytes()),
            json!("2024-01-01")
        );
        assert_eq!(
            decode(&Type::DATE, &i32::MAX.to_be_bytes()),
            json!("infinity")
        );
    }

    #[test]
    fn time_and_timetz() {
        let t = (13 * 3600 + 5 * 60 + 9) * MICROS_PER_SEC + 250_000;
        assert_eq!(decode(&Type::TIME, &t.to_be_bytes()), json!("13:05:09.25"));
        assert_eq!(
            decode(&Type::TIME, &(86_400 * MICROS_PER_SEC).to_be_bytes()),
            json!("24:00:00")
        );
        let mut tz = (3600 * MICROS_PER_SEC).to_be_bytes().to_vec();
        tz.extend_from_slice(&(-7200i32).to_be_bytes()); // UTC+02:00
        assert_eq!(decode(&Type::TIMETZ, &tz), json!("01:00:00+02:00"));
        let mut tz = 0i64.to_be_bytes().to_vec();
        tz.extend_from_slice(&(18000i32).to_be_bytes()); // UTC-05:00
        assert_eq!(decode(&Type::TIMETZ, &tz), json!("00:00:00-05:00"));
    }

    #[test]
    fn intervals() {
        assert_eq!(interval_to_string(0, 0, 0), "00:00:00");
        assert_eq!(interval_to_string(0, 0, 14), "1 year 2 mons");
        assert_eq!(
            interval_to_string(14_706_789_000, 3, 14),
            "1 year 2 mons 3 days 04:05:06.789"
        );
        assert_eq!(interval_to_string(0, 1, 0), "1 day");
        assert_eq!(
            interval_to_string(-3_600 * MICROS_PER_SEC, 0, 0),
            "-01:00:00"
        );
        assert_eq!(
            interval_to_string(3_600 * MICROS_PER_SEC, -1, 0),
            "-1 days +01:00:00"
        );
        assert_eq!(
            interval_to_string(360_000 * MICROS_PER_SEC, 0, 0),
            "100:00:00"
        );
        let mut raw = (1_000_000i64).to_be_bytes().to_vec();
        raw.extend_from_slice(&2i32.to_be_bytes());
        raw.extend_from_slice(&(-1i32).to_be_bytes());
        assert_eq!(
            decode(&Type::INTERVAL, &raw),
            json!("-1 mons +2 days +00:00:01")
        );
    }

    #[test]
    fn network_types() {
        assert_eq!(
            decode(&Type::INET, &[2, 32, 0, 4, 10, 0, 0, 1]),
            json!("10.0.0.1")
        );
        assert_eq!(
            decode(&Type::INET, &[2, 24, 0, 4, 192, 168, 1, 5]),
            json!("192.168.1.5/24")
        );
        assert_eq!(
            decode(&Type::CIDR, &[2, 8, 1, 4, 10, 0, 0, 0]),
            json!("10.0.0.0/8")
        );
        let mut v6 = vec![3, 128, 0, 16];
        v6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        assert_eq!(decode(&Type::INET, &v6), json!("::1"));
        assert_eq!(
            decode(&Type::MACADDR, &[0x08, 0x00, 0x2b, 0x01, 0x02, 0x03]),
            json!("08:00:2b:01:02:03")
        );
    }

    fn array_raw(elem_oid: u32, dims: &[i32], elems: &[Option<Vec<u8>>]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(dims.len() as i32).to_be_bytes());
        v.extend_from_slice(&i32::from(elems.iter().any(Option::is_none)).to_be_bytes());
        v.extend_from_slice(&elem_oid.to_be_bytes());
        for d in dims {
            v.extend_from_slice(&d.to_be_bytes());
            v.extend_from_slice(&1i32.to_be_bytes());
        }
        for e in elems {
            match e {
                None => v.extend_from_slice(&(-1i32).to_be_bytes()),
                Some(b) => {
                    v.extend_from_slice(&(b.len() as i32).to_be_bytes());
                    v.extend_from_slice(b);
                }
            }
        }
        v
    }

    #[test]
    fn arrays() {
        let raw = array_raw(
            23,
            &[3],
            &[
                Some(1i32.to_be_bytes().to_vec()),
                None,
                Some(3i32.to_be_bytes().to_vec()),
            ],
        );
        assert_eq!(decode(&Type::INT4_ARRAY, &raw), json!([1, null, 3]));
        // int8 inside arrays keeps the string rule
        let raw = array_raw(20, &[1], &[Some(i64::MAX.to_be_bytes().to_vec())]);
        assert_eq!(
            decode(&Type::INT8_ARRAY, &raw),
            json!(["9223372036854775807"])
        );
        // text[]
        let raw = array_raw(25, &[2], &[Some(b"a".to_vec()), Some(b"b,c".to_vec())]);
        assert_eq!(decode(&Type::TEXT_ARRAY, &raw), json!(["a", "b,c"]));
        // empty array
        let raw = array_raw(23, &[], &[]);
        assert_eq!(decode(&Type::INT4_ARRAY, &raw), json!([]));
        // 2 x 2
        let e = |n: i32| Some(n.to_be_bytes().to_vec());
        let raw = array_raw(23, &[2, 2], &[e(1), e(2), e(3), e(4)]);
        assert_eq!(decode(&Type::INT4_ARRAY, &raw), json!([[1, 2], [3, 4]]));
    }

    #[test]
    fn corrupt_arrays_do_not_panic() {
        assert_eq!(decode(&Type::INT4_ARRAY, &[0, 0]), json!("<_int4>"));
        // claims 2^30 elements
        let mut raw = Vec::new();
        raw.extend_from_slice(&1i32.to_be_bytes());
        raw.extend_from_slice(&0i32.to_be_bytes());
        raw.extend_from_slice(&23u32.to_be_bytes());
        raw.extend_from_slice(&(1i32 << 30).to_be_bytes());
        raw.extend_from_slice(&1i32.to_be_bytes());
        assert_eq!(decode(&Type::INT4_ARRAY, &raw), json!("<_int4>"));
    }

    #[test]
    fn ranges_bits_and_lsn() {
        // [1,10) as int4range: flags = lb inclusive (0x02)
        let mut raw = vec![0x02u8];
        raw.extend_from_slice(&4i32.to_be_bytes());
        raw.extend_from_slice(&1i32.to_be_bytes());
        raw.extend_from_slice(&4i32.to_be_bytes());
        raw.extend_from_slice(&10i32.to_be_bytes());
        assert_eq!(decode(&Type::INT4_RANGE, &raw), json!("[1,10)"));
        assert_eq!(decode(&Type::INT4_RANGE, &[0x01]), json!("empty"));
        let mut bits = 5i32.to_be_bytes().to_vec();
        bits.push(0b1010_0000);
        assert_eq!(decode(&Type::VARBIT, &bits), json!("10100"));
        assert_eq!(
            decode(&Type::PG_LSN, &0x0000_0001_0000_00A0u64.to_be_bytes()),
            json!("1/A0")
        );
    }
}

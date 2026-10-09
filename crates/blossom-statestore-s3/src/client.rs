//! The S3 requests the store makes, signed (SigV4) and sent over ureq: get, head, put (conditional), delete, and
//! list (ListObjectsV2).

use std::time::Duration;

use blossom_statestore::StateError;
use ureq::Agent;

use crate::sigv4::{self, Credentials, Request};

/// The largest response body read (a value is at most this).
const BODY_LIMIT: u64 = 1 << 30;

/// Where the bucket is, and how its objects are addressed.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// `http` or `https`.
    pub scheme: String,
    /// The service's host, with its port when not the scheme's.
    pub host: String,
    pub bucket: String,
    pub region: String,
    /// `https://host/bucket/key` (MinIO, and most S3-compatible stores) rather than `https://bucket.host/key`.
    pub path_style: bool,
}

/// An answer.
pub struct Resp {
    pub status: u16,
    pub etag: Option<String>,
    /// The `x-amz-meta-blossom-version` header (the manifest's version).
    pub meta_version: Option<String>,
    pub body: Vec<u8>,
}

/// How a request failed before it had an answer.
#[derive(Debug)]
pub enum Failure {
    /// Not sent (it could not connect): nothing happened.
    NotSent(String),
    /// Sent, but no answer came: it may have taken effect.
    NoAnswer(String),
}

impl Failure {
    pub fn unavailable(self) -> StateError {
        match self {
            Failure::NotSent(m) | Failure::NoAnswer(m) => StateError::Unavailable(format!("s3: {m}")),
        }
    }
}

pub struct Client {
    agent: Agent,
    pub endpoint: Endpoint,
    creds: Credentials,
}

/// Seconds since the epoch, for the signature's date (a request's time, not a node's).
#[allow(clippy::disallowed_methods)] // a request's signing time, not a node's clock
pub fn wall_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Milliseconds since the epoch (blobs' ages for the collector).
#[allow(clippy::disallowed_methods)] // a store's housekeeping time, not a node's clock
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Client {
    pub fn new(endpoint: Endpoint, creds: Credentials) -> Client {
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .max_idle_connections_per_host(32)
            .build()
            .new_agent();
        Client { agent, endpoint, creds }
    }

    /// The request path (encoded) and the URL for `key` (empty: the bucket itself).
    fn locate(&self, key: &str) -> (String, String) {
        let e = &self.endpoint;
        let encoded = sigv4::uri_encode(key, true);
        if e.path_style {
            let path = if key.is_empty() {
                format!("/{}", e.bucket)
            } else {
                format!("/{}/{encoded}", e.bucket)
            };
            (path.clone(), format!("{}://{}{path}", e.scheme, e.host))
        } else {
            let path = format!("/{encoded}");
            (path.clone(), format!("{}://{}.{}{path}", e.scheme, e.bucket, e.host))
        }
    }

    fn host(&self) -> String {
        if self.endpoint.path_style {
            self.endpoint.host.clone()
        } else {
            format!("{}.{}", self.endpoint.bucket, self.endpoint.host)
        }
    }

    /// Sends one signed request. `headers` are extra headers (signed too), names lower-case.
    pub fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(String, String)],
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<Resp, Failure> {
        let (path, url) = self.locate(key);
        let qs = sigv4::canonical_query(query);
        let url = if qs.is_empty() { url } else { format!("{url}?{qs}") };
        let payload = sigv4::sha256_hex(body);
        let date = sigv4::amz_date(wall_secs());
        let mut signed: Vec<(String, String)> = vec![
            ("host".into(), self.host()),
            ("x-amz-content-sha256".into(), payload.clone()),
            ("x-amz-date".into(), date.clone()),
        ];
        if let Some(t) = &self.creds.session_token {
            signed.push(("x-amz-security-token".into(), t.clone()));
        }
        for (k, v) in headers {
            signed.push(((*k).to_string(), v.clone()));
        }
        let auth = sigv4::authorization(
            &self.creds,
            &self.endpoint.region,
            &date,
            &Request {
                method,
                path: &path,
                query,
                headers: &signed,
                payload_sha256: &payload,
            },
        );
        let mut req = ureq::http::Request::builder().method(method).uri(&url);
        for (k, v) in signed.iter().filter(|(k, _)| k != "host") {
            req = req.header(k.as_str(), v.as_str());
        }
        req = req.header("authorization", auth);
        let req = req
            .body(body.to_vec())
            .map_err(|e| Failure::NotSent(format!("building a request for {url}: {e}")))?;
        let mut res = self.agent.run(req).map_err(|e| match e {
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => Failure::NotSent(format!("{url}: {e}")),
            ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
                Failure::NotSent(format!("{url}: {e}"))
            }
            other => Failure::NoAnswer(format!("{url}: {other}")),
        })?;
        let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let (etag, meta_version) = (header("etag"), header("x-amz-meta-blossom-version"));
        let status = res.status().as_u16();
        let body = if method == "HEAD" {
            Vec::new()
        } else {
            res.body_mut()
                .with_config()
                .limit(BODY_LIMIT)
                .read_to_vec()
                .map_err(|e| Failure::NoAnswer(format!("{url}: reading the answer: {e}")))?
        };
        Ok(Resp {
            status,
            etag,
            meta_version,
            body,
        })
    }
}

/// An S3 error answer's code and message, for reports.
pub fn error_text(r: &Resp) -> String {
    let body = String::from_utf8_lossy(&r.body);
    let code = tag(&body, "Code").unwrap_or_default();
    let message = tag(&body, "Message").unwrap_or_default();
    if code.is_empty() && message.is_empty() {
        format!("HTTP {}", r.status)
    } else {
        format!("HTTP {} {code}: {message}", r.status)
    }
}

/// The text of the first `<name>…</name>` in `xml`, entities decoded.
pub fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let len = xml.get(start..)?.find(&close)?;
    Some(unescape(xml.get(start..start + len)?))
}

/// Every `<name>…</name>` in `xml`, in order.
pub fn tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let Some(after) = rest.get(i + open.len()..) else { break };
        let Some(j) = after.find(&close) else { break };
        if let Some(inner) = after.get(..j) {
            out.push(inner);
        }
        rest = after.get(j + close.len()..).unwrap_or("");
    }
    out
}

/// XML's five entities and numeric character references.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or(""));
        let after = rest.get(i..).unwrap_or("");
        let Some(end) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let entity = after.get(1..end).unwrap_or("");
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(e.get(2..).unwrap_or(""), 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e.get(1..).and_then(|n| n.parse().ok()).and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(after.get(..=end).unwrap_or("")),
        }
        rest = after.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Milliseconds since the epoch of an S3 timestamp (`2026-10-09T20:41:36.345Z`).
pub fn parse_time_ms(s: &str) -> Option<u64> {
    let num = |a: usize, b: usize| s.get(a..b).and_then(|t| t.parse::<u64>().ok());
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let ms = match s.get(19..20) {
        Some(".") => {
            let frac: String = s.get(20..)?.chars().take_while(char::is_ascii_digit).collect();
            let three: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
            three.parse::<u64>().ok()?
        }
        _ => 0,
    };
    let days = days_from_civil(y, mo, d)?;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: u64, m: u64, d: u64) -> Option<u64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1970 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe).checked_sub(719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml() {
        let x = "<R><Key>a&amp;b</Key><Key>c&#47;d</Key><IsTruncated>false</IsTruncated></R>";
        assert_eq!(tags(x, "Key"), ["a&amp;b", "c&#47;d"]);
        assert_eq!(tag(x, "Key").as_deref(), Some("a&b"));
        assert_eq!(unescape("c&#47;d&#x41;&lt;"), "c/dA<");
        assert_eq!(tag(x, "Missing"), None);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_time_ms("2013-05-24T00:00:00Z"), Some(1_369_353_600_000));
        assert_eq!(parse_time_ms("2000-02-29T12:00:01.5Z"), Some(951_825_601_500));
        assert_eq!(parse_time_ms("nonsense"), None);
    }
}

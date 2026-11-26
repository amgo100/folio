//! The requests the server takes: parsing them, and the checks of the
//! session token, the `Host` header and the websocket's origin.

use std::fmt::Write as _;

/// The longest request head the server reads.
pub(crate) const MAX_HEAD: usize = 16 * 1024;

/// The most headers a request may have.
const MAX_HEADERS: usize = 64;

/// A new session token: 32 random bytes, in hexadecimal.
pub(crate) fn token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    let mut text = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    Ok(text)
}

/// A parsed request head.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// The path, without the query.
    pub(crate) path: String,
    /// The query's parameters, in order, undecoded.
    pub(crate) query: Vec<(String, String)>,
    /// The headers, names in lower case.
    pub(crate) headers: Vec<(String, String)>,
}

impl Request {
    /// Parses a request head; `Ok(None)` when more bytes are needed.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Option<(Self, usize)>, ()> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        let len = match request.parse(bytes) {
            Ok(httparse::Status::Complete(len)) => len,
            Ok(httparse::Status::Partial) => return Ok(None),
            Err(_) => return Err(()),
        };
        let target = request.path.ok_or(())?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (k.to_owned(), v.to_owned())
            })
            .collect();
        let headers = request
            .headers
            .iter()
            .map(|h| {
                let value = String::from_utf8_lossy(h.value).trim().to_owned();
                (h.name.to_ascii_lowercase(), value)
            })
            .collect();
        Ok(Some((
            Self {
                method: request.method.ok_or(())?.to_owned(),
                path: path.to_owned(),
                query,
                headers,
            },
            len,
        )))
    }

    /// The value of a header, by its lower-case name, when it is there
    /// once.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        let mut values = self.headers.iter().filter(|(n, _)| n == name);
        let first = values.next()?;
        values.next().is_none().then_some(first.1.as_str())
    }

    /// The value of a query parameter, when it is there once.
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        let mut values = self.query.iter().filter(|(n, _)| n == name);
        let first = values.next()?;
        values.next().is_none().then_some(first.1.as_str())
    }

    /// Whether the request carries the session token.
    pub(crate) fn has_token(&self, token: &str) -> bool {
        self.param("token")
            .is_some_and(|given| same(given.as_bytes(), token.as_bytes()))
    }

    /// Whether the `Host` header names this server, on its port.
    pub(crate) fn host_is_local(&self, port: u16) -> bool {
        self.header("host").is_some_and(|host| {
            local_hosts(port)
                .iter()
                .any(|h| h.eq_ignore_ascii_case(host))
        })
    }

    /// Whether the websocket's origin is the preview's own, or there is
    /// none, as from a program rather than a page in a browser.
    pub(crate) fn origin_allowed(&self, port: u16) -> bool {
        let origins: Vec<&str> = self
            .headers
            .iter()
            .filter(|(n, _)| n == "origin")
            .map(|(_, v)| v.as_str())
            .collect();
        match origins.as_slice() {
            [] => true,
            [origin] => local_hosts(port)
                .iter()
                .any(|host| origin.eq_ignore_ascii_case(&format!("http://{host}"))),
            _ => false,
        }
    }

    /// Whether the request asks for a websocket.
    pub(crate) fn is_upgrade(&self) -> bool {
        self.header("upgrade")
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
            && self.header("connection").is_some_and(|v| {
                v.split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
    }
}

/// The hosts, with the port, that name the server; without it too on
/// port 80, which browsers leave out.
fn local_hosts(port: u16) -> Vec<String> {
    let mut hosts = vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    if port == 80 {
        hosts.extend(["127.0.0.1".into(), "localhost".into()]);
    }
    hosts
}

/// Compares two byte strings in time that depends only on their lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A response with a body.
pub(crate) fn response(
    status: &str,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// A response that says what is wrong, in plain text.
pub(crate) fn error(status: &str) -> Vec<u8> {
    response(status, "text/plain; charset=utf-8", &[], status.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Request {
        Request::parse(text.as_bytes()).unwrap().unwrap().0
    }

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (token().unwrap(), token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn checks() {
        let request = parse(
            "GET /ws?token=abc&x=1 HTTP/1.1\r\nHost: 127.0.0.1:8000\r\n\
             Origin: http://localhost:8000\r\nUpgrade: websocket\r\n\
             Connection: keep-alive, Upgrade\r\n\r\n",
        );
        assert_eq!(request.path, "/ws");
        assert!(request.has_token("abc"));
        assert!(!request.has_token("abd"));
        assert!(!request.has_token("ab"));
        assert!(request.host_is_local(8000));
        assert!(!request.host_is_local(8001));
        assert!(request.origin_allowed(8000));
        assert!(!request.origin_allowed(8001));
        assert!(request.is_upgrade());

        let default_port =
            parse("GET / HTTP/1.1\r\nHost: localhost\r\nOrigin: http://127.0.0.1\r\n\r\n");
        assert!(default_port.host_is_local(80));
        assert!(default_port.origin_allowed(80));
        assert!(!default_port.host_is_local(8080));
        assert!(!default_port.origin_allowed(8080));

        let foreign = parse(
            "GET / HTTP/1.1\r\nHost: evil.example:8000\r\nOrigin: http://evil.example\r\n\r\n",
        );
        assert!(!foreign.host_is_local(8000));
        assert!(!foreign.origin_allowed(8000));
        assert!(!foreign.has_token(""));

        let twice = parse("GET /?token=a&token=a HTTP/1.1\r\nHost: localhost:1\r\n\r\n");
        assert!(!twice.has_token("a"));
        assert!(twice.origin_allowed(1));
    }

    #[test]
    fn partial_and_bad_heads() {
        assert_eq!(Request::parse(b"GET / HTTP/1.1\r\nHost: a"), Ok(None));
        assert!(Request::parse(b"\x00\x01 nonsense\r\n\r\n").is_err());
    }
}

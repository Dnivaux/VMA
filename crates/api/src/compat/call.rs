//! A call of the compat API: a function and its parameters, as the HTTP query string and the
//! TCP `FUNCTION` line give them (`api-compat.md` § 1–2).

/// `Function=Fade&Input=2&Duration=500`, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub function: String,
    params: Vec<(String, String)>,
}

impl Call {
    pub fn new(function: impl Into<String>, query: &str) -> Self {
        Self {
            function: function.into(),
            params: pairs(query),
        }
    }

    /// From an HTTP query string carrying `Function`; `None` without it.
    pub fn from_query(query: &str) -> Option<Self> {
        let params = pairs(query);
        let function = params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("function"))
            .map(|(_, v)| v.clone())?;
        Some(Self { function, params })
    }

    /// The value of `name` (any case), if given and not empty.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

fn pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

/// URL decoding: `%XX` and `+` (space); a malformed escape is kept as written.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(value) = hex {
                    out.push(value);
                    i += 2;
                } else {
                    out.push(b);
                }
            }
            _ => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_decoded() {
        let call = Call::from_query("Function=SetInputName&Input=2&Value=Cam%C3%A9ra+1").unwrap();
        assert_eq!(call.function, "SetInputName");
        assert_eq!(call.param("input"), Some("2"));
        assert_eq!(call.param("VALUE"), Some("Caméra 1"));
        assert_eq!(call.param("Duration"), None);
        assert!(Call::from_query("Input=2").is_none());
        let tcp = Call::new("fadeToBlack", "");
        assert_eq!(tcp.param("Input"), None);
        assert_eq!(
            Call::new("x", "Value=100%25&Bad=%zz").param("value"),
            Some("100%")
        );
        assert_eq!(Call::new("x", "Bad=%zz").param("bad"), Some("%zz"));
    }
}

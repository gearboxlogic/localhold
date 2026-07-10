use axum::http::{HeaderMap, header::AUTHORIZATION};

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let mut fields = value.split_ascii_whitespace();
    let scheme = fields.next()?;
    let token = fields.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || fields.next().is_some() {
        return None;
    }
    Some(token)
}

pub(crate) fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    bearer_token(headers).is_some_and(|token| token == expected)
}

/// Read a principal asserted by a trusted reverse proxy.
///
/// Callers must only use this after establishing a trust boundary that prevents
/// clients from reaching the endpoint directly or supplying this header.
pub(crate) fn trusted_proxy_principal<'a>(headers: &'a HeaderMap, header_name: &str) -> Option<&'a str> {
    let principal = headers.get(header_name)?.to_str().ok()?.trim();
    (!principal.is_empty()).then_some(principal)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{bearer_matches, bearer_token, trusted_proxy_principal};

    #[test]
    fn parses_bearer_scheme_case_insensitively() {
        let mut headers = HeaderMap::new();
        let _previous = headers.insert(AUTHORIZATION, HeaderValue::from_static("bEaReR secret"));

        assert_eq!(bearer_token(&headers), Some("secret"));
        assert!(bearer_matches(&headers, "secret"));
    }

    #[test]
    fn rejects_malformed_authorization_values() {
        for value in ["secret", "Basic secret", "Bearer", "Bearer secret extra"] {
            let mut headers = HeaderMap::new();
            let _previous = headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
            assert_eq!(bearer_token(&headers), None, "value should be rejected: {value}");
        }
    }

    #[test]
    fn parses_only_non_empty_trusted_proxy_principals() {
        let mut headers = HeaderMap::new();
        let _previous = headers.insert("x-localhold-principal", HeaderValue::from_static("  alice  "));
        assert_eq!(trusted_proxy_principal(&headers, "x-localhold-principal"), Some("alice"));

        let _previous = headers.insert("x-localhold-principal", HeaderValue::from_static("  "));
        assert_eq!(trusted_proxy_principal(&headers, "x-localhold-principal"), None);
        assert_eq!(trusted_proxy_principal(&headers, "x-other-principal"), None);
    }
}

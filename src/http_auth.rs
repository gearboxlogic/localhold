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

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{bearer_matches, bearer_token};

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
}

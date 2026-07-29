#[cfg(all(feature = "lib-violation", not(debug_assertions)))]
/// Deliberately violates the release-only production unwrap policy when enabled.
pub fn unwrap_violation(value: Option<u8>) -> u8 {
    value.unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_policy_allows_normal_test_unwraps() {
        fn unwrap_test_value(value: Option<u8>) -> u8 {
            value.unwrap()
        }

        assert_eq!(unwrap_test_value(Some(1_u8)), 1_u8);
    }
}

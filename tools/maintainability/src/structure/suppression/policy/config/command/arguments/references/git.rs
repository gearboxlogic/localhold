pub(super) fn alias_configuration_is_opaque(arguments: &[String]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let configuration = if matches!(argument.as_str(), "-c" | "--config-env") {
            index += 1;
            arguments.get(index).map(String::as_str)
        } else if let Some(configuration) = argument.strip_prefix("--config-env=") {
            Some(configuration)
        } else {
            argument.strip_prefix("-c").filter(|configuration| !configuration.is_empty())
        };
        if configuration.is_some_and(is_alias_configuration) {
            return true;
        }
        index += 1;
    }
    false
}

fn is_alias_configuration(configuration: &str) -> bool {
    configuration
        .split_once('=')
        .map_or(configuration, |(name, _)| name)
        .trim()
        .to_ascii_lowercase()
        .starts_with("alias.")
}

#[cfg(test)]
mod tests {
    use super::alias_configuration_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn command_producing_alias_configuration_is_opaque() {
        assert!(alias_configuration_is_opaque(&arguments(&["-c", "alias.lint=!sh quality/lint.txt", "lint"])));
        assert!(alias_configuration_is_opaque(&arguments(&["-cALIAS.lint=!sh quality/lint.txt", "lint"])));
        assert!(alias_configuration_is_opaque(&arguments(&["--config-env=alias.lint=LINT_ALIAS", "lint"])));
        assert!(alias_configuration_is_opaque(&arguments(&["--config-env", "alias.lint=LINT_ALIAS", "lint"])));
        assert!(!alias_configuration_is_opaque(&arguments(&["-c", "core.autocrlf=false", "status"])));
    }
}

pub(super) fn dispatch_is_opaque(command: &str, arguments: &[String]) -> bool {
    matches!(command, "ssh-keygen" | "ssh-keygen.exe") && pkcs11_provider_is_selected(arguments)
}

fn pkcs11_provider_is_selected(arguments: &[String]) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| argument.strip_prefix('-').is_some_and(|options| !options.starts_with('-') && options.contains('D')))
}

#[cfg(test)]
mod tests {
    use super::dispatch_is_opaque;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn ssh_keygen_pkcs11_provider_loading_fails_closed() {
        assert!(dispatch_is_opaque("ssh-keygen", &arguments(&["-D", "quality/lint.so"])));
        assert!(dispatch_is_opaque("ssh-keygen.exe", &arguments(&["-Dquality/lint.dll"])));
        assert!(dispatch_is_opaque("ssh-keygen", &arguments(&["-vvDquality/lint.so"])));
        assert!(!dispatch_is_opaque("ssh-keygen", &arguments(&["-lf", "quality/id.pub"])));
        assert!(!dispatch_is_opaque("ssh-keyscan", &arguments(&["-D", "quality/lint.so"])));
    }
}

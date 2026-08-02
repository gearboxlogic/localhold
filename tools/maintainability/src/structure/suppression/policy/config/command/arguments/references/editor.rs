pub(super) fn is_command_capable(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    matches!(
        command,
        "ed" | "emacs"
            | "emacsclient"
            | "evim"
            | "ex"
            | "gvim"
            | "gvimdiff"
            | "nano"
            | "nvim"
            | "pico"
            | "red"
            | "rgview"
            | "rgvim"
            | "rview"
            | "rvim"
            | "vi"
            | "view"
            | "vim"
            | "vimdiff"
    ) || command.starts_with("vim.")
}

#[cfg(test)]
mod tests {
    use super::is_command_capable;

    #[test]
    fn scripted_editor_families_fail_closed() {
        for command in [
            "ed",
            "emacs",
            "emacsclient.exe",
            "ex",
            "gvim",
            "nano.exe",
            "nvim",
            "pico",
            "red",
            "rgvim",
            "rvim",
            "vi",
            "view",
            "vim",
            "vim.basic",
            "vimdiff.exe",
        ] {
            assert!(is_command_capable(command), "{command}");
        }
        for command in ["edit", "notepad.exe", "rustdoc", "view-file"] {
            assert!(!is_command_capable(command), "{command}");
        }
    }
}

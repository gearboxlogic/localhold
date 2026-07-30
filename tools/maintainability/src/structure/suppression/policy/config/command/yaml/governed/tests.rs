use super::validate;

const JOB: &str = r#"  dependency-unsafe-linux:
    runs-on: ubuntu-latest
    steps:
      - name: Install reviewed Rust toolchain
        run: rustup toolchain install 1.97.0 --profile minimal --component clippy --component rustfmt
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
      - name: Run dependency unsafe gate
        run: |
          if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
            printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
            exit 1
          fi
          ./script/check-maintainability-bootstrap.sh --maintainability
      - if: failure()
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
"#;

fn workflow() -> String {
    let windows_job = JOB
        .replace("dependency-unsafe-linux", "dependency-unsafe-windows")
        .replace("ubuntu-latest", "windows-latest")
        .replacen(
            "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
            "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0\n        env:\n          GIT_CONFIG_COUNT: '1'\n          GIT_CONFIG_KEY_0: core.autocrlf\n          GIT_CONFIG_VALUE_0: 'false'",
            1,
        );
    format!("name: CI\non: push\njobs:\n{JOB}{windows_job}")
}

fn assert_rejected(source: &str) {
    assert!(validate(".github/workflows/ci.yml", source).is_err());
}

#[test]
fn governed_gate_steps_are_unconditional_and_platform_bound() {
    let accepted = workflow();
    validate(".github/workflows/ci.yml", &accepted).expect("two unconditional gates");

    let conditional_step = accepted.replacen("        run: |", "        if: false\n        run: |", 1);
    assert_rejected(&conditional_step);

    let conditional_job = accepted.replacen("    runs-on:", "    if: false\n    runs-on:", 1);
    assert_rejected(&conditional_job);

    let non_failing_step = accepted.replacen("        run: |", "        continue-on-error: true\n        run: |", 1);
    assert_rejected(&non_failing_step);

    let non_failing_job = accepted.replacen("    runs-on:", "    continue-on-error: true\n    runs-on:", 1);
    assert_rejected(&non_failing_job);

    let duplicate_linux = accepted.replace("dependency-unsafe-windows", "dependency-unsafe-linux");
    assert_rejected(&duplicate_linux);

    let wrong_runner = accepted.replacen("windows-latest", "ubuntu-latest", 1);
    assert_rejected(&wrong_runner);
}

#[test]
fn governed_invocation_comes_from_the_exact_run_scalar() {
    let accepted = workflow();
    let renamed = accepted.replacen("Run dependency unsafe gate", "Skip dependency unsafe gate", 1);
    assert_rejected(&renamed);

    let comment_spoof = accepted.replacen(
        "      - name: Run dependency unsafe gate\n        run: |",
        "      - name: Run dependency unsafe gate # ./script/check-maintainability-bootstrap.sh --maintainability\n        run: true",
        1,
    );
    assert_rejected(&comment_spoof);

    let heredoc_spoof = accepted.replacen(
        "          ./script/check-maintainability-bootstrap.sh --maintainability",
        "          cat <<'EOF'\n          ./script/check-maintainability-bootstrap.sh --maintainability\n          EOF",
        1,
    );
    assert_rejected(&heredoc_spoof);

    let neutralized_gate = accepted.replacen(
        "          ./script/check-maintainability-bootstrap.sh --maintainability",
        "          ./script/check-maintainability-bootstrap.sh --maintainability || true",
        1,
    );
    assert_rejected(&neutralized_gate);
}

#[test]
fn governed_jobs_have_a_closed_isolated_step_sequence() {
    let accepted = workflow();
    let preceding_command = accepted.replacen(
        "      - name: Run dependency unsafe gate",
        "      - run: ./quality/background-helper\n      - name: Run dependency unsafe gate",
        1,
    );
    assert_rejected(&preceding_command);

    let unreviewed_action = accepted.replacen(
        "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
        "      - uses: attacker/example@1111111111111111111111111111111111111111",
        1,
    );
    assert_rejected(&unreviewed_action);

    let unreviewed_toolchain = accepted.replacen("rustup toolchain install 1.97.0", "mise install", 1);
    assert_rejected(&unreviewed_toolchain);

    let bare_sequence_step = accepted.replacen(
        "      - name: Install reviewed Rust toolchain",
        "      -\n        run: /usr/bin/nohup /tmp/background-helper\n      - name: Install reviewed Rust toolchain",
        1,
    );
    assert_rejected(&bare_sequence_step);
}

#[test]
fn windows_checkout_requires_exact_line_ending_configuration() {
    let accepted = workflow();
    let missing = accepted.replacen("          GIT_CONFIG_COUNT: '1'\n", "", 1);
    assert_rejected(&missing);

    let altered = accepted.replacen("          GIT_CONFIG_VALUE_0: 'false'", "          GIT_CONFIG_VALUE_0: 'true'", 1);
    assert_rejected(&altered);

    let duplicate = accepted.replacen("          GIT_CONFIG_COUNT: '1'", "          GIT_CONFIG_COUNT: '1'\n          GIT_CONFIG_COUNT: '1'", 1);
    assert_rejected(&duplicate);

    let linux_environment = accepted.replacen(
        "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
        "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0\n        env:\n          GIT_CONFIG_COUNT: '1'\n          GIT_CONFIG_KEY_0: core.autocrlf\n          GIT_CONFIG_VALUE_0: 'false'",
        1,
    );
    assert_rejected(&linux_environment);
}

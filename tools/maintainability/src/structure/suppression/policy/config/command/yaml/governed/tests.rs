use super::{WINDOWS_SHELL, validate};

const JOB: &str = r#"  dependency-unsafe-linux:
    runs-on: ubuntu-latest
    defaults:
      run:
        shell: /usr/bin/env -u BASH_ENV -u ENV -u GCONV_PATH -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}
    steps:
      - name: Install reviewed Rust toolchain
        env:
          RUSTUP_DIST_SERVER: https://static.rust-lang.org
          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup
          RUSTUP_UPDATE_ROOT: https://static.rust-lang.org/rustup
        run: rustup toolchain install 1.97.0 --profile minimal --component clippy --component rustfmt
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Run dependency unsafe gate
        id: audit
        env:
          BASH_ENV: ''
          GCONV_PATH: ''
          SHELLOPTS: ''
          LD_AUDIT: ''
          LD_LIBRARY_PATH: ''
          LD_PRELOAD: ''
          LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256: ${{ hashFiles('script/check-maintainability-bootstrap.sh') }}
          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}
          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup
        run: |
          if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
            printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
            exit 1
          fi
          ./script/check-maintainability-bootstrap.sh --maintainability
      - name: Upload dependency audit evidence on failure
        if: failure() && steps.audit.outcome == 'failure'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: dependency-unsafe-linux-${{ github.sha }}
          path: target/dependency-unsafe/actual-linux
          if-no-files-found: warn
          retention-days: 7
"#;

const QUALITY_JOB: &str = r"  check:
    runs-on: ubuntu-latest
    steps:
      - name: Run CI gate
        run: just check-quality
";

fn workflow() -> String {
    let windows_job = JOB
        .replace("dependency-unsafe-linux", "dependency-unsafe-windows")
        .replace("ubuntu-latest", "windows-latest")
        .replace(
            "shell: /usr/bin/env -u BASH_ENV -u ENV -u GCONV_PATH -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}",
            "shell: 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"$ErrorActionPreference = ''Stop''; $env:BASH_ENV = $null; $env:ENV = $null; $env:GCONV_PATH = $null; $env:SHELLOPTS = $null; $env:LD_AUDIT = $null; $env:LD_LIBRARY_PATH = $null; $env:LD_PRELOAD = $null; & ''C:\\Program Files\\Git\\bin\\bash.exe'' --noprofile --norc -e -o pipefail ''{0}''; exit $LASTEXITCODE\"'",
        )
        .replace("target/dependency-unsafe/actual-linux", "target/dependency-unsafe/actual-windows")
        .replacen(
            "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
            "      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0\n        env:\n          GIT_CONFIG_COUNT: '1'\n          GIT_CONFIG_KEY_0: core.autocrlf\n          GIT_CONFIG_VALUE_0: 'false'",
            1,
        );
    format!("name: CI\non: push\njobs:\n{JOB}{windows_job}{QUALITY_JOB}")
}

#[test]
fn windows_shell_uses_a_whitespace_free_runner_command() {
    let (command, arguments) = WINDOWS_SHELL.split_once(' ').expect("custom shell command and arguments");
    assert_eq!(command, r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    assert!(arguments.contains("'{0}'"));
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

    let dependent_job = accepted.replacen("    runs-on:", "    needs: skipped-setup\n    runs-on:", 1);
    assert_rejected(&dependent_job);

    let service_container = accepted.replacen(
        "    runs-on:",
        "    services:\n      attacker:\n        image: attacker/example\n        volumes:\n          - ${{ github.workspace }}:/workspace\n    runs-on:",
        1,
    );
    assert_rejected(&service_container);

    let sh_default = accepted.replacen(
        "        shell: /usr/bin/env -u BASH_ENV -u ENV -u GCONV_PATH -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}",
        "        shell: bash",
        1,
    );
    assert_rejected(&sh_default);

    let missing_default = accepted.replacen(
        "    defaults:\n      run:\n        shell: /usr/bin/env -u BASH_ENV -u ENV -u GCONV_PATH -u SHELLOPTS -u LD_AUDIT -u LD_LIBRARY_PATH -u LD_PRELOAD /usr/bin/bash --noprofile --norc -e -o pipefail {0}\n",
        "",
        1,
    );
    assert_rejected(&missing_default);
}

#[test]
fn required_quality_gate_is_present_unconditional_and_failing() {
    let accepted = workflow();
    validate(".github/workflows/ci.yml", &accepted).expect("unconditional quality gate");

    for altered in [
        accepted.replacen("        run: just check-quality", "        if: false\n        run: just check-quality", 1),
        accepted.replacen("        run: just check-quality", "        continue-on-error: true\n        run: just check-quality", 1),
        accepted.replacen("  check:\n    runs-on:", "  check:\n    if: false\n    runs-on:", 1),
        accepted.replacen("  check:\n    runs-on:", "  check:\n    continue-on-error: true\n    runs-on:", 1),
        accepted.replacen("  check:\n    runs-on:", "  check:\n    needs: skipped-setup\n    runs-on:", 1),
        accepted.replacen("      - name: Run CI gate\n        run: just check-quality\n", "", 1),
        accepted.replacen("run: just check-quality", "run: true", 1),
        accepted.replacen("name: Run CI gate", "name: Optional CI gate", 1),
    ] {
        assert_rejected(&altered);
    }
}

#[test]
fn governed_shell_clears_startup_channels_before_bash_starts() {
    let accepted = workflow();
    let unsanitized = accepted.replacen("-u BASH_ENV -u ENV ", "", 1);
    assert_rejected(&unsanitized);

    let unsanitized_loader = accepted.replacen("-u LD_PRELOAD ", "", 1);
    assert_rejected(&unsanitized_loader);

    let unsanitized_conversion_loader = accepted.replacen("-u GCONV_PATH ", "", 1);
    assert_rejected(&unsanitized_conversion_loader);

    let inherited_shell_options = accepted.replacen("-u SHELLOPTS ", "", 1);
    assert_rejected(&inherited_shell_options);

    let unsanitized_windows = accepted.replacen("$env:BASH_ENV = $null; ", "", 1);
    assert_rejected(&unsanitized_windows);

    let unsanitized_windows_conversion_loader = accepted.replacen("$env:GCONV_PATH = $null; ", "", 1);
    assert_rejected(&unsanitized_windows_conversion_loader);

    let inherited_windows_shell_options = accepted.replacen("$env:SHELLOPTS = $null; ", "", 1);
    assert_rejected(&inherited_windows_shell_options);

    let step_override = accepted.replacen("        id: audit", "        id: audit\n        shell: bash", 1);
    assert_rejected(&step_override);
}

#[test]
fn governed_invocation_comes_from_the_exact_run_scalar() {
    let accepted = workflow();
    let renamed = accepted.replacen("Run dependency unsafe gate", "Skip dependency unsafe gate", 1);
    assert_rejected(&renamed);

    let comment_spoof = accepted.replacen(
        "      - name: Run dependency unsafe gate",
        "      - name: Skip dependency unsafe gate # ./script/check-maintainability-bootstrap.sh --maintainability",
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

    let explicit_indentation_spoof = accepted.replacen(
        r#"        run: |
          if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
            printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
            exit 1
          fi
          ./script/check-maintainability-bootstrap.sh --maintainability"#,
        r#"        run: |1
         #if [[ "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256" != "$LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256" ]]; then
         #  printf 'maintainability bootstrap differs from the workflow-reviewed digest\n' >&2
         #  exit 1
         #fi
         #./script/check-maintainability-bootstrap.sh --maintainability"#,
        1,
    );
    assert_rejected(&explicit_indentation_spoof);
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

    let untrusted_distribution = accepted.replacen("https://static.rust-lang.org", "https://attacker.invalid", 1);
    assert_rejected(&untrusted_distribution);

    let shared_rustup_home = accepted.replacen("${{ runner.temp }}/localhold-rustup", "${{ env.RUSTUP_HOME }}", 1);
    assert_rejected(&shared_rustup_home);

    let missing_gate_rustup_home = accepted.replacen(
        "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}\n          RUSTUP_HOME: ${{ runner.temp }}/localhold-rustup\n        run: |",
        "          LOCALHOLD_MAINTAINABILITY_BASE_REV: ${{ github.event.pull_request.base.sha || (github.event.before != '0000000000000000000000000000000000000000' && github.event.before) || github.sha }}\n        run: |",
        1,
    );
    assert_rejected(&missing_gate_rustup_home);

    let bare_sequence_step = accepted.replacen(
        "      - name: Install reviewed Rust toolchain",
        "      -\n        run: /usr/bin/nohup /tmp/background-helper\n      - name: Install reviewed Rust toolchain",
        1,
    );
    assert_rejected(&bare_sequence_step);

    let wrong_upload_condition = accepted.replacen("        if: failure() && steps.audit.outcome == 'failure'", "        if: false", 1);
    assert_rejected(&wrong_upload_condition);

    let missing_gate_id = accepted.replacen("        id: audit\n", "", 1);
    assert_rejected(&missing_gate_id);

    let wrong_upload_path = accepted.replacen(
        "          path: target/dependency-unsafe/actual-linux",
        "          path: target/dependency-unsafe/missing",
        1,
    );
    assert_rejected(&wrong_upload_path);
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

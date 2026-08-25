use super::Analysis;

const RELEASE: &[&str] = &[
    "$archive = Get-ChildItem dist/*.zip | Select-Object -First 1",
    "$expected = \"hold \" + $env:GITHUB_REF_NAME.Substring(1)",
    "if ($actual -ne $expected) { throw \"unexpected version: $actual\" }",
    "./hold-smoke.exe --help | Out-Null",
];

const RELEASE_SMOKE: &[&str] = &[
    "New-Item -ItemType Directory -Path release | Out-Null",
    "foreach ($line in Get-Content release/SHA256SUMS) {",
    "if ($line -notmatch '^([0-9a-f]{64})\\s+\\./(.+)$') {",
    "$actual = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant()",
    "if ($actual -ne $expected) {",
    "if ($verified -lt 2) { throw \"expected at least two release asset checksums, got $verified\" }",
    "$version = $env:RELEASE_TAG.Substring(1)",
    "if ($actualVersion -ne \"hold $version\") { throw \"unexpected version: $actualVersion\" }",
    "./hold-smoke.exe --help | Out-Null",
    "if ($LASTEXITCODE -ne 0) { throw \"help command failed\" }",
    "$responseLines = @($request | ./hold-smoke.exe 2>\"$env:RUNNER_TEMP/mcp-stderr.log\")",
    "$response = $responseLines | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object id -eq 1 | Select-Object -First 1",
    "if ($null -eq $response) { throw \"missing MCP initialize response\" }",
    "if ($response.result.serverInfo.name -ne \"localhold\") { throw \"unexpected MCP server name\" }",
    "if ($response.result.serverInfo.title -ne \"LocalHold\") { throw \"unexpected MCP server title\" }",
    "if ($response.result.serverInfo.version -ne $version) { throw \"unexpected MCP server version\" }",
];

const CI: &[&str] = &[
    "$archive = Get-ChildItem \"$env:RUNNER_TEMP/release/*.zip\" | Select-Object -First 1",
    "if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {",
    "if ($LASTEXITCODE -ne 0) { throw \"archived Windows binary version command failed\" }",
    "./hold-smoke.exe --help | Out-Null",
    "if ($LASTEXITCODE -ne 0) { throw \"archived Windows binary help command failed\" }",
];

pub(super) fn accepts(path: &str, source_is_reviewed: bool, analysis: &Analysis) -> bool {
    if !analysis.unresolved() {
        return true;
    }
    if !source_is_reviewed {
        return false;
    }
    let statements = match path {
        ".github/workflows/release.yml" => RELEASE,
        ".github/workflows/release-smoke.yml" => RELEASE_SMOKE,
        ".github/workflows/ci.yml" => CI,
        _ => return false,
    };
    analysis.unresolved_statements.iter().all(|statement| statements.contains(&statement.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_source_does_not_blanket_authorize_new_statements() {
        for source in [
            "$value = $(./quality/payload.ps1)",
            "$value = input | ./quality/payload.ps1",
            "$value = & { ./quality/payload.ps1 }",
            "& $tool ./quality/payload.ps1",
            "Write-Output safe; ./quality/payload.ps1",
            ". ./quality/payload.ps1",
            "ForEach-Object './quality/payload.ps1'",
            "Get-Content input > ./quality/output",
        ] {
            let analysis = super::super::analyze_execution_commands(source);
            assert!(analysis.unresolved(), "{source}");
            assert!(!accepts(".github/workflows/release.yml", true, &analysis), "{source}");
        }
    }

    #[test]
    fn exact_profiles_cover_every_current_reviewed_workflow_statement() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for path in [".github/workflows/release.yml", ".github/workflows/release-smoke.yml", ".github/workflows/ci.yml"] {
            let source = std::fs::read_to_string(repository.join(path)).expect("workflow source");
            for command in crate::structure::suppression::policy::config::command::yaml::powershell_run_commands(path, &source) {
                let analysis = super::super::analyze_execution_commands(&command);
                assert!(accepts(path, true, &analysis), "{path}: {:?}", analysis.unresolved_statements);
            }
        }
    }
}

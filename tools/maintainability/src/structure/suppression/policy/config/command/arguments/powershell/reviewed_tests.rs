use super::{analyze_execution_commands, has_constructed_rust_arguments};

#[test]
fn constructed_rust_arguments_fail_closed() {
    assert!(has_constructed_rust_arguments("cargo clippy -- ('-' + 'A') warnings"));
    assert!(has_constructed_rust_arguments("cargo clippy -- \"$(Get-LintLevel)\" warnings"));
    assert!(has_constructed_rust_arguments(
        "Start-Process -FilePath ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('Y2FyZ28='))) -ArgumentList 'clippy','--','-A','warnings' -Wait"
    ));
    assert!(has_constructed_rust_arguments("[System.Diagnostics.Process]::Start($tool, $arguments).WaitForExit()"));
    assert!(has_constructed_rust_arguments("[Diagnostics.Process]::new()"));
    assert!(has_constructed_rust_arguments("[scriptblock]::Create($source).Invoke()"));
    assert!(has_constructed_rust_arguments("[System.Management.Automation.ScriptBlock]::Create($source).Invoke()"));
    assert!(analyze_execution_commands("$assembly = [System.Reflection.Assembly]::LoadFrom('quality/payload.dll')\n$assembly.EntryPoint.Invoke($null, @())").unresolved());
    assert!(has_constructed_rust_arguments("#Requires -Modules ./quality/payload.dll\nWrite-Output safe"));
    assert!(has_constructed_rust_arguments("#requires -PSSnapin Untrusted.SnapIn\nWrite-Output safe"));
    assert!(has_constructed_rust_arguments("[System.IO.File]::Copy('quality/Justfile', 'Justfile', $true)"));
    assert!(has_constructed_rust_arguments("[IO.Compression.ZipFile]::ExtractToDirectory($archive, '.')"));
    assert!(has_constructed_rust_arguments("New-Alias x ('Invoke-' + 'Expression'); x $decoded"));
    assert!(has_constructed_rust_arguments("Set-Alias x Invoke-Expression; x $decoded"));
    assert!(!has_constructed_rust_arguments("$scriptblock = 'inert'; Write-Output $scriptblock"));
    assert!(!has_constructed_rust_arguments("Write-Output '[scriptblock]::Create($source)'"));
    assert!(!has_constructed_rust_arguments("cargo clippy -- '-A' warnings"));
    assert!(!has_constructed_rust_arguments("Write-Output '(cargo clippy -- -A warnings)'"));
    assert!(!has_constructed_rust_arguments("Write-Output 'Start-Process cargo'"));
    assert!(!has_constructed_rust_arguments("Write-Output '[System.Diagnostics.Process]::Start($tool)'"));
    assert!(!has_constructed_rust_arguments("Write-Output 'New-Alias x Invoke-Expression'"));
    assert!(!has_constructed_rust_arguments("# Start-Process cargo"));
    assert!(!has_constructed_rust_arguments("# [System.Diagnostics.Process]::Start($tool)"));
    assert!(!has_constructed_rust_arguments("# Requires -Modules ./quality/payload.dll"));
    assert!(!has_constructed_rust_arguments("Write-Output '#Requires -Modules ./quality/payload.dll'"));
    assert!(!has_constructed_rust_arguments("@'\n#Requires -Modules ./quality/payload.dll\n'@"));
    assert!(!has_constructed_rust_arguments("@'\nStart-Process cargo\n'@"));
    assert!(!has_constructed_rust_arguments("@'\n[System.Diagnostics.Process]::Start($tool)\n'@"));
    assert!(!has_constructed_rust_arguments("$actual = (Get-FileHash -Algorithm SHA256 $path).Hash"));
    assert!(!has_constructed_rust_arguments("$name = [IO.Path]::GetFileNameWithoutExtension($archive.Name)"));
    assert!(!has_constructed_rust_arguments("Write-Output '[System.IO.File]::Copy($source, $destination)'"));
    assert!(!has_constructed_rust_arguments("# [System.IO.File]::Copy($source, $destination)"));
}

# OpenClaw Provider Integration Tests
# Run manually: powershell -ExecutionPolicy Bypass -File test-openclaw-cli.ps1

$env:Path = "C:\Program Files\nodejs" + ";" + $env:Path

$passed = 0
$failed = 0

function Test-Case {
    param([string]$Name, [scriptblock]$Test)
    Write-Host "Testing: $Name" -NoNewline
    try {
        & $Test
        Write-Host " ? PASSED" -ForegroundColor Green
        $script:passed++
    } catch {
        Write-Host " ? FAILED: $_" -ForegroundColor Red
        $script:failed++
    }
}

# Test 1: openclaw --version
Test-Case "openclaw --version" {
    $output = openclaw --version 2>&1
    if (-not ($output -match "2026")) { throw "Expected version to contain '2026', got: $output" }
}

# Test 2: openclaw agent --help
Test-Case "openclaw agent --help" {
    $output = openclaw agent --help 2>&1
    if (-not ($output -match "--message")) { throw "Expected --message option" }
    if (-not ($output -match "--session-id")) { throw "Expected --session-id option" }
    if (-not ($output -match "--json")) { throw "Expected --json option" }
}

# Test 3: openclaw sessions --help
Test-Case "openclaw sessions --help" {
    $output = openclaw sessions --help 2>&1
    if (-not ($output -match "cleanup")) { throw "Expected cleanup subcommand" }
    if (-not ($output -match "--active")) { throw "Expected --active option" }
}

# Test 4: openclaw status
Test-Case "openclaw status" {
    $output = openclaw status 2>&1
    if ([string]::IsNullOrEmpty($output)) { throw "Expected status output" }
}

# Summary
Write-Host "`n========================================"
Write-Host "Results: $passed passed, $failed failed" -ForegroundColor $(if ($failed -eq 0) { "Green" } else { "Red" })
Write-Host "========================================"

if ($failed -gt 0) { exit 1 }
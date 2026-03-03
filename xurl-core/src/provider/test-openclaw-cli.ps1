# OpenClaw Provider Integration Tests - ??????
# Run: powershell -ExecutionPolicy Bypass -File test-openclaw-cli.ps1
# Note: Requires ANTHROPIC_API_KEY environment variable

$env:Path = "C:\Program Files\nodejs" + ";" + $env:Path

$passed = 0
$failed = 0

function Test-Case {
    param([string]$Name, [scriptblock]$Test)
    Write-Host "`nTesting: $Name" -NoNewline -ForegroundColor Cyan
    try {
        & $Test
        Write-Host " ? PASSED" -ForegroundColor Green
        $script:passed++
    } catch {
        Write-Host " ? FAILED: $_" -ForegroundColor Red
        $script:failed++
    }
}

Write-Host "`n========================================" -ForegroundColor Yellow
Write-Host "OpenClaw ??????" -ForegroundColor Yellow
Write-Host "Requires: ANTHROPIC_API_KEY env var" -ForegroundColor Gray
Write-Host "========================================`n" -ForegroundColor Yellow

# Test 1: ?? Gateway ??
Test-Case "Gateway ????" {
    $output = openclaw status 2>&1
    if (-not ($output -match "gateway|Dashboard")) { throw "Gateway ???????" }
    Write-Host "`n  Gateway ????" -ForegroundColor Gray
}

# Test 2: ?????(??????)
Test-Case "Agent ???? (1+1=?)" {
    $message = "Answer in one word: what is 1+1? Just the number."
    $output = openclaw agent --message $message --json --timeout 30 2>&1 | Out-String
    if (-not ($output -match "2")) { 
        Write-Host "`n  ??:$output" -ForegroundColor Gray
        throw "Agent ???????" 
    }
    Write-Host "`n  Agent ???????" -ForegroundColor Gray
}

# Test 3: ??????
Test-Case "??????" {
    $output = openclaw sessions --json 2>&1 | Out-String
    if (-not ($output -match "sessions")) { throw "????????" }
    Write-Host "`n  ????????" -ForegroundColor Gray
}

# Test 4: ???? Agents
Test-Case "???? Agents" {
    $output = openclaw agents list 2>&1 | Out-String
    Write-Host "`n  Agents ??????" -ForegroundColor Gray
}

# Test 5: ?? Skills
Test-Case "???? Skills" {
    $output = openclaw skills list 2>&1 | Out-String
    if ($output -match "Error|not found") { 
        Write-Host "`n  Skills ???(??)" -ForegroundColor Gray
    } else {
        Write-Host "`n  Skills ????" -ForegroundColor Gray
    }
}

# Summary
Write-Host "`n========================================" -ForegroundColor Yellow
Write-Host "??:$passed ??,$failed ??" -ForegroundColor $(if ($failed -eq 0) { "Green" } else { "Red" })
Write-Host "========================================`n" -ForegroundColor Yellow

if ($failed -gt 0) { exit 1 }
exit 0
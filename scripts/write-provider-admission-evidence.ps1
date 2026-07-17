param(
    [Parameter(Mandatory = $true)][string]$EvidencePath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-f]{40}$')][string]$SourceCommit,
    [Parameter(Mandatory = $true)][string]$RunId,
    [Parameter(Mandatory = $true)][string]$RunAttempt,
    [Parameter(Mandatory = $true)][string]$Executor,
    [Parameter(Mandatory = $true)][string]$OperatingSystem,
    [Parameter(Mandatory = $true)][string]$Architecture
)
$ErrorActionPreference = 'Stop'
$capturePath = Join-Path $EvidencePath 'suite.capture.json'
if (-not (Test-Path -LiteralPath $capturePath -PathType Leaf)) { throw 'provider admission capture is missing' }
$capture = Get-Content -LiteralPath $capturePath -Raw | ConvertFrom-Json
if ($capture.format -ne 'eva.release.command_capture.v1' -or $capture.capture_id -ne 'provider-admission' -or $capture.outcome -ne 'success' -or $capture.exit_code -ne 0) { throw 'provider admission capture did not complete successfully' }
$stdoutPath = Join-Path $EvidencePath $capture.stdout.path
$stdout = Get-Content -LiteralPath $stdoutPath -Raw
foreach ($scenario in @('two_processes_have_one_winner_for_capacity_one','crashed_process_reservation_is_reclaimed_only_after_expiry')) {
    if (-not $stdout.Contains($scenario)) { throw "provider admission capture missing scenario: $scenario" }
}
$captureDigest = (Get-FileHash -LiteralPath $capturePath -Algorithm SHA256).Hash.ToLowerInvariant()
$completedAt = [DateTime]::UtcNow.ToString('o')
$manifest = [ordered]@{
    schema = 'eva.provider-admission-evidence.v1'
    source_commit = $SourceCommit
    run_id = $RunId
    run_attempt = $RunAttempt
    executor = $Executor
    environment = [ordered]@{ os = $OperatingSystem; arch = $Architecture }
    completed_at = $completedAt
    capture = [ordered]@{ path = 'suite.capture.json'; digest = "sha256:$captureDigest" }
    scenarios = @('capacity_one_two_process_competition','crash_expiry_reclaim','stale_identity_fence')
}
$output = Join-Path $EvidencePath 'provider-admission.evidence.json'
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $output -Encoding utf8
$roundTrip = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
if ($roundTrip.source_commit -ne $SourceCommit -or $roundTrip.executor -ne $Executor -or $roundTrip.capture.digest -ne "sha256:$captureDigest") { throw 'provider admission evidence readback mismatch' }

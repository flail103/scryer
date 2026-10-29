$ErrorActionPreference = "Stop"
Import-Module Microsoft.PowerShell.Utility
. "$PSScriptRoot/windows-startup-auth.ps1"

function Assert-That($Condition, [string]$Message) {
  if (-not $Condition) { throw $Message }
}

# Replace only the HTTP boundary; exercise the real setup and proof handling.
function Invoke-WebRequest {
  param($Uri, $Headers, $WebSession, $TimeoutSec, [switch]$SkipHttpErrorCheck)
  Assert-That ($Uri -eq "http://127.0.0.1:18080/authless-client") "Unexpected proof URL"
  Assert-That ($WebSession -eq $script:session) "Proof request lost its cookie session"
  Assert-That $SkipHttpErrorCheck "Setup refusals must be classified by their reason code"
  return $script:responses.Dequeue()
}

function Invoke-RestMethod {
  param($Uri, $Method, $ContentType, $Body, $WebSession, $TimeoutSec)
  Assert-That ($Uri -eq "http://127.0.0.1:18080/graphql") "Unexpected setup URL"
  Assert-That ($Method -eq "Post") "Setup must use POST"
  Assert-That ($ContentType -eq "application/json") "Setup must use JSON"
  Assert-That ($WebSession -eq $script:session) "Setup request lost its cookie session"
  $script:mutations.Add(($Body | ConvertFrom-Json))
  return $script:setupResult
}

function Test-ProofCase {
  param([object[]]$Responses, [string]$Output, [bool]$Fails, [int]$MutationCount, $SetupResult)
  $script:responses = [System.Collections.Generic.Queue[object]]::new()
  foreach ($response in $Responses) { $script:responses.Enqueue($response) }
  $script:mutations = [System.Collections.Generic.List[object]]::new()
  $script:setupResult = $SetupResult
  Set-Content -LiteralPath $stderrPath -Value $Output
  $failed = $false
  try {
    $proof = Get-ScryerStartupProof -BaseUrl "http://127.0.0.1:18080" -Session $session -StderrPath $stderrPath
  } catch {
    $failed = $true
  }
  Assert-That ($failed -eq $Fails) "Unexpected success/failure outcome"
  Assert-That ($script:mutations.Count -eq $MutationCount) "Unexpected setup mutation count"
  if (-not $Fails) { Assert-That ($proof -eq "browser-proof") "Expected the issued browser proof" }
}

$session = [Microsoft.PowerShell.Commands.WebRequestSession]::new()
$stderrPath = [System.IO.Path]::GetTempFileName()
$challenge = @{ StatusCode = 403; Content = '{"errors":[{"extensions":{"reason":"INITIAL_AUTH_REQUIRED"}}]}' }
$issued = @{ StatusCode = 200; Content = '{"proof":"browser-proof"}' }
$completed = @{ data = @{ completeInitialAuthentication = @{ completed = $true } } }
try {
  Test-ProofCase -Responses @($issued) -Output "" -Fails $false -MutationCount 0
  Test-ProofCase -Responses @($challenge, $issued) -Output "Scryer one-time setup code: ACD-347" -Fails $false -MutationCount 1 -SetupResult $completed
  $input = $script:mutations[0].variables.input
  Assert-That ($input.mode -eq "NO_AUTHENTICATION") "Expected explicit no-authentication selection"
  Assert-That ($input.acknowledgeNoAuthentication -eq $true) "Expected acknowledgement"
  Assert-That ($input.setupCode -eq "ACD-347") "Expected the operator code"

  Test-ProofCase -Responses @($challenge, $issued) -Output "Scryer one-time setup code: ACD-347`r`nScryer one-time setup code: HJK-479`r`n" -Fails $false -MutationCount 1 -SetupResult $completed
  Assert-That ($script:mutations[0].variables.input.setupCode -eq "HJK-479") "Expected the current process's latest code"
  Test-ProofCase -Responses @($challenge) -Output "ordinary output" -Fails $true -MutationCount 0
  Test-ProofCase -Responses @(@{ StatusCode = 403; Content = '{"errors":[{"extensions":{"reason":"OTHER_REFUSAL"}}]}' }) -Output "Scryer one-time setup code: ACD-347" -Fails $true -MutationCount 0
  Test-ProofCase -Responses @($challenge) -Output "Scryer one-time setup code: ACD-347" -Fails $true -MutationCount 1 -SetupResult @{ errors = @(@{ extensions = @{ reason = "INITIAL_AUTH_CODE_INVALID" } }) }
  Test-ProofCase -Responses @($challenge) -Output "Scryer one-time setup code: ACD-347" -Fails $true -MutationCount 1 -SetupResult @{ data = @{ completeInitialAuthentication = @{ completed = $false } } }
  Test-ProofCase -Responses @(@{ StatusCode = 200; Content = '{}' }) -Output "" -Fails $true -MutationCount 0
  Test-ProofCase -Responses @(@{ StatusCode = 503; Content = '{}' }) -Output "" -Fails $true -MutationCount 0
  Test-ProofCase -Responses @(@{ StatusCode = 200; Content = [System.Text.Encoding]::UTF8.GetBytes('{"proof":"browser-proof"}') }) -Output "" -Fails $false -MutationCount 0
  Write-Host "Windows startup authentication: 10 cases passed"
} finally {
  Remove-Item -LiteralPath $stderrPath
}

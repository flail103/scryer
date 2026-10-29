# Complete the fresh-install decision through the same operator proof as the UI.
# Keep the captured stderr private: it contains the one-time setup credential.
function Get-ScryerStartupProof {
  param(
    [string]$BaseUrl,
    [Microsoft.PowerShell.Commands.WebRequestSession]$Session,
    [string]$StderrPath
  )

  $request = @{
    Uri = "$BaseUrl/authless-client"
    Headers = @{ Accept = "application/json" }
    WebSession = $Session
    TimeoutSec = 5
    SkipHttpErrorCheck = $true
  }
  $response = Invoke-WebRequest @request
  if ($response.StatusCode -eq 403) {
    $refusal = $response.Content | ConvertFrom-Json
    if ($refusal.errors.extensions.reason -notcontains "INITIAL_AUTH_REQUIRED") {
      throw "Authless web client proof was refused without an initial-setup challenge"
    }

    $codes = [regex]::Matches(
      (Get-Content -LiteralPath $StderrPath -Raw),
      '(?m)^Scryer one-time setup code: ([A-Z2-9]{3}-[A-Z2-9]{3})\r?$'
    )
    if ($codes.Count -eq 0) {
      throw "Current Scryer process has not emitted its setup code"
    }
    $payload = @{
      query = 'mutation($input: CompleteInitialAuthenticationInput!) { completeInitialAuthentication(input: $input) { completed } }'
      variables = @{ input = @{
        mode = "NO_AUTHENTICATION"
        acknowledgeNoAuthentication = $true
        setupCode = $codes[$codes.Count - 1].Groups[1].Value
      } }
    } | ConvertTo-Json -Depth 5 -Compress
    $setup = Invoke-RestMethod -Uri "$BaseUrl/graphql" -Method Post -ContentType "application/json" -Body $payload -WebSession $Session -TimeoutSec 5
    if ($setup.errors -or $setup.data.completeInitialAuthentication.completed -ne $true) {
      throw "Initial authentication decision did not complete"
    }
    $response = Invoke-WebRequest @request
  }

  if ($response.StatusCode -ne 200) {
    throw "Authless web client proof returned HTTP $($response.StatusCode)"
  }
  $content = if ($response.Content -is [byte[]]) {
    [System.Text.Encoding]::UTF8.GetString($response.Content)
  } else {
    [string]$response.Content
  }
  $proof = ($content | ConvertFrom-Json).proof
  if ([string]::IsNullOrWhiteSpace($proof)) {
    throw "Authless web client proof response did not include a proof"
  }
  return $proof
}

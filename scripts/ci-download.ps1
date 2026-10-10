# Download with a deadline per attempt and a few attempts, failing with what was being fetched.
# Invoke-WebRequest has no deadline of its own, so a stalled connection holds the job for as
# long as the job lets it. Dot-source this file; it only defines the function.
function Get-WithRetry {
  param(
    [Parameter(Mandatory)][string]$Uri,
    [Parameter(Mandatory)][string]$OutFile,
    [Parameter(Mandatory)][string]$What,
    [int]$Attempts = 4,
    [int]$TimeoutSec = 90
  )
  for ($i = 1; $i -le $Attempts; $i++) {
    try {
      Invoke-WebRequest $Uri -OutFile $OutFile -TimeoutSec $TimeoutSec
      return
    } catch {
      Write-Output "$What, attempt $i of ${Attempts}: $_"
      if ($i -eq $Attempts) {
        Write-Output "::error::could not download $What from $Uri after $Attempts attempts: $_"
        throw
      }
      Start-Sleep -Seconds (5 * $i)
    }
  }
}

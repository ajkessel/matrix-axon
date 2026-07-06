# Start axon from a source checkout: check prerequisites, start a local
# developer Postgres when needed, and run Cargo.
# Uses a local Postgres instance if one is reachable; otherwise starts one via
# Docker Compose and tears it down on exit.

# --- Prerequisites ---

function Test-Cmd($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

function Invoke-Compose {
    & docker compose --project-directory $PSScriptRoot `
        -f (Join-Path $PSScriptRoot 'docker-compose.yml') @args
}

function Test-LocalPostgres([string]$host, [int]$port) {
    if (Test-Cmd 'pg_isready') {
        $null = & pg_isready -h $host -p $port -q 2>&1
        return $LASTEXITCODE -eq 0
    }
    try {
        $result = Test-NetConnection -ComputerName $host -Port $port `
            -InformationLevel Quiet -WarningAction SilentlyContinue 2>$null
        return [bool]$result
    } catch {
        return $false
    }
}

# --- Determine target early ---

$target = if ($args.Count -gt 0) { $args[0] } else { 'server' }
$extraArgs = if ($args.Count -gt 1) { $args[1..($args.Count - 1)] } else { @() }
$package = switch ($target) {
    'server' { 'axon-server' }
    'tui'    { 'axon-tui' }
    'clean'  { 'clean' }
    default  {
        Write-Host "Error: unknown target '$target'. Valid targets: server (default), tui, clean."
        exit 1
    }
}

# --- Prerequisites ---

if ($package -ne 'clean') {
    if (-not (Test-Cmd 'cargo')) {
        Write-Host "Error: 'cargo' is not installed or not in PATH."
        Write-Host ""
        Write-Host "Install Rust (includes cargo):"
        Write-Host "  winget install Rustlang.Rustup"
        Write-Host "  Or visit: https://www.rust-lang.org/tools/install"
        exit 1
    }
}

# Docker prerequisite check is deferred until after .env is loaded, so that
# POSTGRES_HOST/PORT are set before local-Postgres detection runs.

# --- Load .env if present ---

$loadedVars = @{}
$envFile = Join-Path $PSScriptRoot '.env'
if (Test-Path $envFile) {
    Get-Content $envFile | ForEach-Object {
        if ($_ -match '^\s*#' -or $_ -match '^\s*$') { return }
        $parts = $_ -split '=', 2
        if ($parts.Count -eq 2) {
            $name = $parts[0].Trim()
            $currentValue = [System.Environment]::GetEnvironmentVariable($name, 'Process')
            if ($name -match '^[A-Za-z_][A-Za-z0-9_]*$' -and
                [string]::IsNullOrEmpty($currentValue)) {
                $loadedVars[$name] = $currentValue
                [System.Environment]::SetEnvironmentVariable($name, $parts[1].Trim(), 'Process')
            }
        }
    }
}

# --- Detect local Postgres (after .env so POSTGRES_HOST/PORT are available) ---

$localPg = $false
if ($package -eq 'axon-server' -or $package -eq 'clean') {
    $pgHost = if ($env:POSTGRES_HOST) { $env:POSTGRES_HOST } else { '127.0.0.1' }
    $pgPort = if ($env:POSTGRES_PORT) { [int]$env:POSTGRES_PORT } else { 5432 }
    $localPg = Test-LocalPostgres $pgHost $pgPort
}

# --- Check Docker prerequisites (only when Docker will actually be used) ---

if (-not $localPg -and ($package -eq 'axon-server' -or $package -eq 'clean')) {
    if (-not (Test-Cmd 'docker')) {
        Write-Host "Error: 'docker' is not installed or not in PATH."
        Write-Host ""
        Write-Host "Install Docker Desktop:"
        Write-Host "  winget install Docker.DockerDesktop"
        Write-Host "  Or visit: https://docs.docker.com/desktop/windows/install/"
        exit 1
    }

    $null = docker info 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Error: Docker is installed but Docker Desktop is not running."
        Write-Host ""
        Write-Host "Start Docker Desktop from the Start menu or system tray and try again."
        exit 1
    }
}

# --- Run ---

if ($package -eq 'clean') {
    if ($localPg) {
        $pgUser = if ($env:POSTGRES_USER) { $env:POSTGRES_USER } else { 'axon' }
        $pgDb   = if ($env:POSTGRES_DB)   { $env:POSTGRES_DB }   else { 'axon' }
        Write-Host "Note: a local Postgres instance is running at ${pgHost}:${pgPort}."
        Write-Host "The 'clean' target only manages the Docker-managed database volume."
        Write-Host "Nothing was changed."
        Write-Host ""
        Write-Host "To drop the database manually, run:"
        Write-Host "  psql -h $pgHost -p $pgPort -U $pgUser -c 'DROP DATABASE IF EXISTS $pgDb;'"
        exit 0
    }
    Write-Host "Warning: this will permanently destroy all Postgres data."
    $answer = Read-Host "Continue? [y/N]"
    if ($answer -match '^[yY]') {
        Invoke-Compose down -v
        exit $LASTEXITCODE
    } else {
        Write-Host "Aborted."
        exit 1
    }
}

$exitCode = 1
try {
    if ($package -eq 'axon-server') {
        if ($localPg) {
            Write-Host "Local Postgres detected at ${pgHost}:${pgPort} — skipping Docker."
        } else {
            Invoke-Compose up -d --wait postgres
            if ($LASTEXITCODE -ne 0) {
                Write-Host "Error: Docker Compose could not start the Postgres service."
                Write-Host "Review the Docker output above; no database reset was attempted."
                exit 1
            }

            $postgresUser = if ($env:POSTGRES_USER) { $env:POSTGRES_USER } else { 'axon' }
            $postgresPassword = if ($env:POSTGRES_PASSWORD) { $env:POSTGRES_PASSWORD } else { 'axon' }
            $postgresDb = if ($env:POSTGRES_DB) { $env:POSTGRES_DB } else { 'axon' }

            $postgresOutput = Invoke-Compose exec -e "PGPASSWORD=$postgresPassword" postgres `
                psql -h localhost -U $postgresUser -d $postgresDb -c "SELECT 1" 2>&1
            if ($LASTEXITCODE -ne 0) {
                $postgresError = $postgresOutput | Out-String
                if ($postgresError -notmatch
                    'password authentication failed|role ".+" does not exist|database ".+" does not exist') {
                    Write-Host "Error: could not run the Postgres credential check:"
                    Write-Host $postgresError.Trim()
                    Write-Host "No database reset was attempted."
                    exit 1
                }

                Write-Host "Error: could not connect to the Compose Postgres service with its configured credentials."
                Write-Host "The database volume was likely initialized with a different password."
                Write-Host ""
                $reset = Read-Host "Reset the database now? This destroys all existing data. [y/N]"
                if ($reset -match '^[yY]') {
                    Invoke-Compose down -v
                    if ($LASTEXITCODE -ne 0) {
                        Write-Host "Error: Docker Compose could not remove the existing database volume."
                        exit 1
                    }
                    Invoke-Compose up -d --wait postgres
                    if ($LASTEXITCODE -ne 0) {
                        Write-Host "Error: Docker Compose could not restart Postgres after the reset."
                        exit 1
                    }
                    $postgresOutput = Invoke-Compose exec -e "PGPASSWORD=$postgresPassword" postgres `
                        psql -h localhost -U $postgresUser -d $postgresDb -c "SELECT 1" 2>&1
                    if ($LASTEXITCODE -ne 0) {
                        Write-Host "Error: Postgres still rejects the configured credentials after the reset."
                        Write-Host (($postgresOutput | Out-String).Trim())
                        exit 1
                    }
                } else {
                    Write-Host "Aborting. Update the Compose Postgres credentials to match the existing"
                    Write-Host "database, or run 'docker compose down -v' from the project directory to reset."
                    exit 1
                }
            }
        }
    }

    $currentAxonConfig = [System.Environment]::GetEnvironmentVariable('AXON_CONFIG', 'Process')
    if ([string]::IsNullOrEmpty($currentAxonConfig)) {
        $defaultConfig = Join-Path $PSScriptRoot 'axon.toml'
        if (Test-Path $defaultConfig) {
            $loadedVars['AXON_CONFIG'] = $currentAxonConfig
            [System.Environment]::SetEnvironmentVariable('AXON_CONFIG', $defaultConfig, 'Process')
        }
    }

    & cargo run --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml') -p $package -- @extraArgs
    $exitCode = $LASTEXITCODE
} finally {
    if ($package -eq 'axon-server' -and -not $localPg) {
        $null = Invoke-Compose down
    }
    foreach ($entry in $loadedVars.GetEnumerator()) {
        [System.Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
}

exit $exitCode

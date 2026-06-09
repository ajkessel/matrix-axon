# Start axon-server: check prerequisites, configure environment, run with Docker,
# and tear down Docker containers on exit.

# --- Prerequisites ---

function Test-Cmd($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

function Invoke-Compose {
    & docker compose --project-directory $PSScriptRoot `
        -f (Join-Path $PSScriptRoot 'docker-compose.yml') @args
}

function New-RandomHex([int]$byteCount) {
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $bytes = New-Object byte[] $byteCount
    $rng.GetBytes($bytes)
    $rng.Dispose()
    return ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
}

if (-not (Test-Cmd 'docker')) {
    Write-Host "Error: 'docker' is not installed or not in PATH."
    Write-Host ""
    Write-Host "Install Docker Desktop:"
    Write-Host "  winget install Docker.DockerDesktop"
    Write-Host "  Or visit: https://docs.docker.com/desktop/windows/install/"
    exit 1
}

if (-not (Test-Cmd 'cargo')) {
    Write-Host "Error: 'cargo' is not installed or not in PATH."
    Write-Host ""
    Write-Host "Install Rust (includes cargo):"
    Write-Host "  winget install Rustlang.Rustup"
    Write-Host "  Or visit: https://www.rust-lang.org/tools/install"
    exit 1
}

$null = docker info 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "Error: Docker is installed but Docker Desktop is not running."
    Write-Host ""
    Write-Host "Start Docker Desktop from the Start menu or system tray and try again."
    exit 1
}

# --- Load .env if present ---

$loadedVars = @{}
$envFile = Join-Path $PSScriptRoot '.env'
$envExample = Join-Path $PSScriptRoot '.env.example'
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

# --- Offer guided configuration when no other database config exists ---

$configFile = if ($env:AXON_CONFIG) {
    $env:AXON_CONFIG
} else {
    Join-Path $PSScriptRoot 'axon.toml'
}

if (-not (Test-Path $envFile) -and (Test-Path $envExample) -and
    -not $env:DATABASE_URL -and -not $env:AXON_DATABASE__URL -and
    -not (Test-Path $configFile)) {
    $answer = Read-Host "No database configuration found. Create .env from .env.example now? [y/N]"
    if ($answer -match '^[yY]') {
        $storeKey = New-RandomHex 32
        $pgPass = New-RandomHex 16
        $content = Get-Content $envExample -Raw
        $content = $content -replace 'AXON_SYNC__STORE_KEY=change-me', "AXON_SYNC__STORE_KEY=$storeKey"
        $content = $content -replace '(?m)^POSTGRES_PASSWORD=.*', "POSTGRES_PASSWORD=$pgPass"
        $content = $content -replace '(?m)^(DATABASE_URL=postgres://[^:]+:)[^@]+(@)', ('${1}' + $pgPass + '${2}')
        [System.IO.File]::WriteAllText(
            $envFile,
            $content,
            [System.Text.UTF8Encoding]::new($false)
        )
        Write-Host "Created .env with generated database and store keys."
        $editor = if ($env:EDITOR) { $env:EDITOR } else { 'notepad' }
        Write-Host "Opening in $editor to complete configuration. Save and close when done."
        if ($env:EDITOR) {
            & $env:EDITOR $envFile
        } else {
            Start-Process notepad $envFile -Wait
        }
        & $PSCommandPath @args
        exit $LASTEXITCODE
    } else {
        Write-Host "Aborted. Configure the database in .env, axon.toml, or the environment, then re-run."
        exit 1
    }
}

# --- Run ---

$target = if ($args.Count -gt 0) { $args[0] } else { 'server' }
$package = switch ($target) {
    'server' { 'axon-server' }
    'tui'    { 'axon-tui' }
    default  {
        Write-Host "Error: unknown target '$target'. Valid targets: server (default), tui."
        exit 1
    }
}

$exitCode = 1
try {
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

    $currentAxonConfig = [System.Environment]::GetEnvironmentVariable('AXON_CONFIG', 'Process')
    if ([string]::IsNullOrEmpty($currentAxonConfig)) {
        $defaultConfig = Join-Path $PSScriptRoot 'axon.toml'
        if (Test-Path $defaultConfig) {
            $loadedVars['AXON_CONFIG'] = $currentAxonConfig
            [System.Environment]::SetEnvironmentVariable('AXON_CONFIG', $defaultConfig, 'Process')
        }
    }

    & cargo run --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml') -p $package
    $exitCode = $LASTEXITCODE
} finally {
    $null = Invoke-Compose down
    foreach ($entry in $loadedVars.GetEnumerator()) {
        [System.Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
}

exit $exitCode

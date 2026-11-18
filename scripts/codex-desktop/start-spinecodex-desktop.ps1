[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$dpiSource = @'
using System;
using System.Runtime.InteropServices;

public static class SpineCodexDpi
{
    [DllImport("user32.dll")]
    public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
}
'@

try {
    Add-Type -TypeDefinition $dpiSource -ErrorAction Stop
    [void][SpineCodexDpi]::SetThreadDpiAwarenessContext([IntPtr](-4))
    Add-Type -AssemblyName System.Windows.Forms -ErrorAction Stop
} catch {
    try {
        Add-Type -AssemblyName System.Windows.Forms -ErrorAction Stop
        [System.Windows.Forms.MessageBox]::Show(
            $_.Exception.Message,
            'SpineCodex Desktop Launcher',
            [System.Windows.Forms.MessageBoxButtons]::OK,
            [System.Windows.Forms.MessageBoxIcon]::Error
        ) | Out-Null
    } catch {
        [Console]::Error.WriteLine($_.Exception.Message)
    }
    exit 1
}

function Show-LaunchError {
    param(
        [Parameter(Mandatory)]
        [string]$Message,

        [int]$ExitCode = 1
    )

    try {
        [System.Windows.Forms.MessageBox]::Show(
            $Message,
            'SpineCodex Desktop Launcher',
            [System.Windows.Forms.MessageBoxButtons]::OK,
            [System.Windows.Forms.MessageBoxIcon]::Error
        ) | Out-Null
    } catch {
        [Console]::Error.WriteLine($Message)
    }

    exit $ExitCode
}

function ConvertTo-SingleQuotedPowerShellLiteral {
    param(
        [Parameter(Mandatory)]
        [string]$Value
    )

    return "'" + $Value.Replace("'", "''") + "'"
}

try {
    $npmCommand = Get-Command npm.cmd -ErrorAction SilentlyContinue
    if ($null -eq $npmCommand) {
        $npmCommand = Get-Command npm -ErrorAction SilentlyContinue
    }
    if ($null -eq $npmCommand) {
        throw 'npm was not found in PATH.'
    }

    $nodeCommand = Get-Command node.exe -ErrorAction SilentlyContinue
    if ($null -eq $nodeCommand) {
        $nodeCommand = Get-Command node -ErrorAction SilentlyContinue
    }
    if ($null -ne $nodeCommand) {
        $nodeExecutable = $nodeCommand.Source
    } else {
        $nodeExecutable = Join-Path (Split-Path -Parent $npmCommand.Source) 'node.exe'
        if (-not (Test-Path -LiteralPath $nodeExecutable -PathType Leaf)) {
            throw 'Node.js was not found in PATH or next to npm.'
        }
    }

    $nodeArchitectureOutput = @(& $nodeExecutable -p 'process.arch')
    $nodeArchitectureExitCode = $LASTEXITCODE
    $nodeArchitecture = [string]($nodeArchitectureOutput | Select-Object -First 1)
    $nodeArchitecture = $nodeArchitecture.Trim()
    if ($nodeArchitectureExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($nodeArchitecture)) {
        throw 'Could not determine the Node.js architecture.'
    }
    switch ($nodeArchitecture) {
        'x64' {
            $nativePackage = '@spinejit\spine-codex-win32-x64'
            $targetTriple = 'x86_64-pc-windows-msvc'
        }
        'arm64' {
            $nativePackage = '@spinejit\spine-codex-win32-arm64'
            $targetTriple = 'aarch64-pc-windows-msvc'
        }
        default {
            throw "Unsupported Node.js architecture: $nodeArchitecture"
        }
    }

    $npmRootOutput = @(& $npmCommand.Source root -g)
    $npmRootExitCode = $LASTEXITCODE
    $npmRoot = [string]($npmRootOutput | Select-Object -First 1)
    $npmRoot = $npmRoot.Trim()
    if ($npmRootExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($npmRoot)) {
        throw 'Could not determine the global npm modules directory.'
    }

    $native = Join-Path $npmRoot (
        '@spinejit\spine-codex\node_modules\' +
        $nativePackage +
        '\vendor\' +
        $targetTriple +
        '\bin\codex.exe'
    )
    if (-not (Test-Path -LiteralPath $native -PathType Leaf)) {
        throw "Could not find the native SpineCodex backend: $native"
    }

    $packages = @(Get-AppxPackage -Name 'OpenAI.Codex' -ErrorAction SilentlyContinue)
    $package = $packages |
        Sort-Object Version -Descending |
        Select-Object -First 1
    if ($null -eq $package) {
        throw 'The official Codex Windows app is not installed.'
    }

    $manifestPath = Join-Path $package.InstallLocation 'AppxManifest.xml'
    $manifest = [xml](Get-Content -Raw -LiteralPath $manifestPath)
    $application = @(
        $manifest.Package.Applications.Application |
            Where-Object { $_.Executable }
    ) | Select-Object -First 1
    if ($null -eq $application) {
        throw 'No executable application was found in the Codex package manifest.'
    }

    $appId = $application.GetAttribute('Id')
    $relativeExecutable = $application.GetAttribute('Executable')
    $appExecutable = Join-Path $package.InstallLocation (
        $relativeExecutable -replace '/', '\'
    )
    $packageRoot = [string]$package.InstallLocation

    $allProcesses = @(Get-CimInstance Win32_Process)
    $running = @(
        $allProcesses | Where-Object {
            $_.ExecutablePath -and
            $_.ExecutablePath.StartsWith(
                $packageRoot,
                [System.StringComparison]::OrdinalIgnoreCase
            ) -and
            (Split-Path -Leaf $_.ExecutablePath) -in @('ChatGPT.exe', 'Codex.exe')
        }
    )
    if ($running.Count -gt 0) {
        Show-LaunchError -ExitCode 2 -Message 'Codex Desktop is already running. Quit it before retrying.'
    }

    $nativeLiteral = ConvertTo-SingleQuotedPowerShellLiteral $native
    $appLiteral = ConvertTo-SingleQuotedPowerShellLiteral $appExecutable
    $launchCommand = (
        '$ErrorActionPreference = ''Stop''; ' +
        '$env:CODEX_CLI_PATH = ' + $nativeLiteral + '; ' +
        '$env:CODEX_SPINE_APP_UI = ''1''; ' +
        'Start-Process -FilePath ' + $appLiteral
    )
    $launchArguments = '-NoProfile -NonInteractive -WindowStyle Hidden -Command "' + $launchCommand + '"'

    $launchParameters = @{
        PackageFamilyName = $package.PackageFamilyName
        AppId = $appId
        Command = 'powershell.exe'
        Args = $launchArguments
    }
    Invoke-CommandInDesktopPackage @launchParameters | Out-Null

    Write-Host "Codex Desktop started with native npm SpineCodex: $native"
} catch {
    Show-LaunchError -Message $_.Exception.Message
}

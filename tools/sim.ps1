[CmdletBinding(PositionalBinding = $true)]
param(
    [Parameter(Position = 0)]
    [ValidateSet("setup", "build", "run", "shot", "send")]
    [string]$Action = "run",

    [string]$Distro = "Ubuntu-22.04",
    [string]$PmsHost = "",
    [ValidateRange(1, 65535)]
    [int]$PmsPort = 32400,
    [string]$RuntimeDir = "",
    [string]$TargetDir = "",
    [string]$AssetDir = "",
    [string]$Output = "",
    [switch]$StageToken,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$InputTokens
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-WslShell {
    param(
        [Parameter(Mandatory = $true)][string]$Script,
        [hashtable]$Environment = @{},
        [switch]$Root
    )

    $arguments = @("-d", $Distro)
    if ($Root) {
        $arguments += @("-u", "root")
    }
    # --exec bypasses WSL's login shell so every following array element stays a literal argv.
    # Without it, values containing $(), backticks or shell metacharacters are expanded once
    # before `env` starts, despite PowerShell having passed them as distinct arguments.
    $arguments += @("--exec", "env")
    foreach ($name in ($Environment.Keys | Sort-Object)) {
        $arguments += "${name}=$($Environment[$name])"
    }
    $arguments += @("bash", "-s")

    # Feed the program on stdin. Passing a multiline `bash -lc` argument through Windows causes
    # wsl.exe to reconstruct quoting and can expand shell variables before bash receives it.
    (($Script -replace "`r", "") + "`n# sim.ps1 end") | & wsl.exe @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "WSL command failed with exit code $LASTEXITCODE."
    }
}

function Get-AbsoluteWindowsPath {
    param([Parameter(Mandatory = $true)][string]$WindowsPath)

    if ([System.IO.Path]::IsPathRooted($WindowsPath)) {
        [System.IO.Path]::GetFullPath($WindowsPath)
    } else {
        # GetFullPath(string) uses the process working directory, which can differ from
        # PowerShell's Set-Location. Resolve relative user arguments against the shell location.
        [System.IO.Path]::GetFullPath((Join-Path (Get-Location).ProviderPath $WindowsPath))
    }
}

function ConvertTo-WslPath {
    param([Parameter(Mandatory = $true)][string]$WindowsPath)

    $absolute = Get-AbsoluteWindowsPath $WindowsPath
    # `--cd` is parsed by wsl.exe itself, so it accepts a Windows directory without feeding its
    # backslashes through a Linux command line. Walk to an existing ancestor so output files and
    # new subdirectories can be translated too, then append their path components in PowerShell.
    $ancestor = $absolute
    $suffix = [System.Collections.Generic.List[string]]::new()
    while (-not (Test-Path -LiteralPath $ancestor -PathType Container)) {
        $leaf = [System.IO.Path]::GetFileName($ancestor)
        if ([string]::IsNullOrEmpty($leaf)) {
            throw "Could not find an existing parent directory for '$absolute'."
        }
        $suffix.Insert(0, $leaf)
        $ancestor = [System.IO.Path]::GetDirectoryName($ancestor)
    }
    $translated = & wsl.exe -d $Distro --cd $ancestor --exec pwd -P
    if ($LASTEXITCODE -ne 0) {
        throw "Could not translate Windows path '$absolute' for $Distro."
    }
    $linuxPath = ($translated | Out-String).Trim().TrimEnd("/")
    foreach ($part in $suffix) {
        $linuxPath += "/$part"
    }
    return $linuxPath
}

function Assert-WslgFastTransport {
    # WSLg can start after a stale SectionFs session with its shared-memory graphics channel
    # disabled. OpenGL still renders at full speed in that state, but Weston copies the surface
    # through the legacy RDP path and the Windows window can update at only a few frames per second.
    try {
        $transport = Invoke-WslShell -Script @'
set -euo pipefail
line=$(grep "RDP backend: use_gfxredir" /mnt/wslg/weston.log 2>/dev/null | tail -n 1)
test -n "$line"
printf '%s\n' "$line"
'@
    } catch {
        throw "Could not read the WSLg graphics transport state for $Distro."
    }
    if (($transport | Out-String) -match 'use_gfxredir\s*=\s*0') {
        throw @"
WSLg started in slow copy mode (use_gfxredir=0). The simulator may report 60 FPS while
the Windows window updates at only a few FPS. Close other WSL work, run `wsl.exe --shutdown`,
then retry this command. The next WSLg start should report use_gfxredir=1.
"@
    }
}

foreach ($directoryOverride in @(
    @{ Name = "RuntimeDir"; Value = $RuntimeDir },
    @{ Name = "TargetDir"; Value = $TargetDir },
    @{ Name = "AssetDir"; Value = $AssetDir }
)) {
    if (-not [string]::IsNullOrWhiteSpace($directoryOverride.Value) -and
        -not $directoryOverride.Value.StartsWith("/")) {
        throw "-$($directoryOverride.Name) must be an absolute Linux path inside WSL."
    }
    if ($directoryOverride.Value.Contains([char]34) -or
        $directoryOverride.Value.Contains("`r") -or
        $directoryOverride.Value.Contains("`n")) {
        throw "-$($directoryOverride.Name) cannot contain double quotes or newlines."
    }
    if ($directoryOverride.Name -eq "TargetDir" -and
        ($directoryOverride.Value.Contains('$') -or
         $directoryOverride.Value.Contains([char]96))) {
        throw "-TargetDir cannot contain dollar signs or backticks because GNU make parses that value."
    }
}

$repoWindows = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$repoLinux = ConvertTo-WslPath $repoWindows
$commonEnvironment = @{
    PLX_REPO = $repoLinux
    PLX_PMS = $PmsHost
    PLX_PORT = $(if ($PSBoundParameters.ContainsKey("PmsPort")) {
        $PmsPort.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    } else { "" })
    PLX_RUNTIME = $RuntimeDir
    PLX_TARGET = $TargetDir
    PLX_ASSETS = $AssetDir
    PLX_STAGE_TOKEN = $(if ($StageToken) { "1" } else { "" })
}

$prepare = @'
set -euo pipefail
. "$HOME/.cargo/env"
repo="$PLX_REPO"
runtime="${PLX_RUNTIME:-$HOME/.local/state/plxnative-sim}"
target="${PLX_TARGET:-$HOME/.cache/plxnative-sim/target}"
assets="${PLX_ASSETS:-$HOME/.local/share/plxnative-sim/assets}"
mkdir -p "$runtime" "$target" "$assets"
# All launch paths use the same build-and-stage operation. The ASS library is
# produced by make, and the process loads it from PLXNATIVE_APP_DIR, not pkg/.
build_simulator() {
    cd "$repo"
    make sim-wsl SIM_TDIR="$target"
    for file in appfont.ttf appfont-bold.ttf appfont-cjk.ttf OFL.txt libass-plx-host.so.0; do
        staged="$assets/$file.$$.new"
        install -m 0644 "$repo/pkg/$file" "$staged"
        mv -f "$staged" "$assets/$file"
    done
}
if [ "${PLX_STAGE_TOKEN:-}" = 1 ]; then
    token=""
    if [ -f "$repo/src/config.local.h" ]; then
        token=$(sed -n 's/^#define[[:space:]]*PMS_TOKEN[[:space:]]*"\([^"]*\)".*/\1/p' "$repo/src/config.local.h" | head -n 1 || true)
    fi
    if [ -z "$token" ]; then
        echo "No PMS_TOKEN was found in src/config.local.h." >&2
        exit 1
    fi
    umask 077
    printf '%s' "$token" > "$runtime/plxnative-token"
    echo "Plex token staged in the private simulator runtime directory."
elif [ -s "$runtime/plxnative-token" ]; then
    echo "Using the token already staged in the simulator runtime directory."
fi
pms="${PLX_PMS:-}"
if [ -z "$pms" ] && [ -f "$repo/src/config.local.h" ]; then
    pms=$(sed -n 's/^#define[[:space:]]*PMS_HOST[[:space:]]*"\([^"]*\)".*/\1/p' "$repo/src/config.local.h" | head -n 1 || true)
fi
port="${PLX_PORT:-}"
if [ -z "$port" ] && [ -f "$repo/src/config.local.h" ]; then
    port=$(sed -n 's/^#define[[:space:]]*PMS_PORT[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$repo/src/config.local.h" | head -n 1 || true)
fi
port="${port:-32400}"
export PLXNATIVE_RUNTIME_DIR="$runtime"
export PLXNATIVE_APP_DIR="$assets"
export PLXNATIVE_WIN=1920x1080
export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-x11}"
'@

$instanceLocks = @'
set -euo pipefail
runtime="${PLX_RUNTIME:-$HOME/.local/state/plxnative-sim}"
target="${PLX_TARGET:-$HOME/.cache/plxnative-sim/target}"
mkdir -p "$runtime" "$target"
exec 9>"$runtime/plxnative-sim.lock"
if ! flock -n 9; then
    echo "Simulator runtime is already in use: $runtime" >&2
    exit 1
fi
exec 8>"$target/plxnative-sim.instance.lock"
if ! flock -n 8; then
    echo "Simulator build artifacts are in use: $target. Use a separate -TargetDir for another instance." >&2
    exit 1
fi
'@

switch ($Action) {
    "setup" {
        Invoke-WslShell -Root -Script @'
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y build-essential cmake pkg-config libsdl2-dev libsdl2-ttf-dev libgl1-mesa-dev mesa-utils curl ca-certificates util-linux
'@
        Invoke-WslShell -Script @'
set -e
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
fi
. "$HOME/.cargo/env"
rustup toolchain install stable --profile minimal
pkg-config --print-errors --exists sdl2 SDL2_ttf gl
renderer=$(glxinfo -B)
printf '%s\n' "$renderer" | sed -n '1,12p'
if printf '%s\n' "$renderer" | grep -Eiq 'llvmpipe|softpipe|Accelerated:[[:space:]]*no'; then
    echo "WSLg is using software OpenGL; update WSL and the Windows GPU driver before running the simulator." >&2
    exit 1
fi
echo "WSLg simulator dependencies are ready."
'@
    }
    "build" {
        Invoke-WslShell -Environment $commonEnvironment -Script ($instanceLocks + "`n" + $prepare + "`n" + @'
build_simulator
echo "Built $target/release/plxnative-sim"
'@)
    }
    "run" {
        Assert-WslgFastTransport
        Invoke-WslShell -Environment $commonEnvironment -Script ($instanceLocks + "`n" + $prepare + "`n" + @'
build_simulator
printf '%s\n' "$$" > "$runtime/plxnative-sim.pid"
exec "$target/release/plxnative-sim" "$pms" "$port"
'@)
    }
    "shot" {
        Assert-WslgFastTransport
        if ([string]::IsNullOrWhiteSpace($Output)) {
            $Output = Join-Path (Get-Location) "plxnative-shot.png"
        }
        $outputAbsolute = Get-AbsoluteWindowsPath $Output
        $commonEnvironment.PLX_OUTPUT = ConvertTo-WslPath $outputAbsolute
        Invoke-WslShell -Environment $commonEnvironment -Script ($instanceLocks + "`n" + $prepare + "`n" + @'
build_simulator
mkdir -p "$(dirname "$PLX_OUTPUT")"
base="$runtime/windows-shot.png"
captured="$runtime/windows-shot-1.png"
rm -f "$base" "$captured"
PLXNATIVE_SHOT="$base" "$target/release/plxnative-sim" "$pms" "$port" &
pid=$!
printf '%s\n' "$pid" > "$runtime/plxnative-sim.pid"
cleanup() {
    kill "$pid" 2>/dev/null || true
    if [ "$(cat "$runtime/plxnative-sim.pid" 2>/dev/null || true)" = "$pid" ]; then
        rm -f "$runtime/plxnative-sim.pid"
    fi
}
trap cleanup EXIT
for _ in $(seq 1 200); do
    [ -p "$runtime/plxnative-remote" ] && break
    kill -0 "$pid" 2>/dev/null || { wait "$pid"; exit $?; }
    sleep 0.05
done
[ -p "$runtime/plxnative-remote" ] || { echo "Simulator remote did not start." >&2; exit 1; }
sleep 5
exec 3<>"$runtime/plxnative-remote"
printf 'shot ' >&3
exec 3>&-
complete=0
for _ in $(seq 1 200); do
    if [ -f "$captured" ]; then
        # save_buffer creates the file before its encoder is finished. The fixed PNG IEND chunk
        # proves the writer closed a complete image before we terminate the simulator.
        trailer=$(tail -c 12 "$captured" 2>/dev/null | od -An -tx1 | tr -d '[:space:]' || true)
        if [ "$trailer" = "0000000049454e44ae426082" ]; then
            complete=1
            break
        fi
    fi
    kill -0 "$pid" 2>/dev/null || { wait "$pid"; exit $?; }
    sleep 0.05
done
[ "$complete" = 1 ] || { echo "Simulator did not produce a complete screenshot." >&2; exit 1; }
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
rm -f "$runtime/plxnative-sim.pid"
trap - EXIT
mv "$captured" "$PLX_OUTPUT"
echo "Wrote $PLX_OUTPUT"
'@)
        Write-Host "Screenshot: $outputAbsolute"
    }
    "send" {
        if ($InputTokens.Count -eq 0) {
            throw "Pass one or more tokens, for example: tools/sim.ps1 send right ok shot"
        }
        foreach ($token in $InputTokens) {
            if ($token -notmatch '^(up|down|left|right|ok|back|play|pause|stop|okdown|okup|shot|ck:-?[0-9]+,-?[0-9]+)$') {
                throw "Unsupported simulator token '$token'."
            }
        }
        $commonEnvironment.PLX_TOKENS = $InputTokens -join " "
        Invoke-WslShell -Environment $commonEnvironment -Script @'
set -euo pipefail
runtime="${PLX_RUNTIME:-$HOME/.local/state/plxnative-sim}"
target="${PLX_TARGET:-$HOME/.cache/plxnative-sim/target}"
fifo="$runtime/plxnative-remote"
pid=$(cat "$runtime/plxnative-sim.pid" 2>/dev/null || true)
actual=$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)
expected=$(readlink -f "$target/release/plxnative-sim" 2>/dev/null || true)
if [ ! -p "$fifo" ] || [ -z "$pid" ] || [ "$actual" != "$expected" ]; then
    echo "The simulator remote is unavailable at $fifo. Start tools/sim.ps1 run first." >&2
    exit 1
fi
exec 3<>"$fifo"
for token in $PLX_TOKENS; do
    printf '%s ' "$token" >&3
done
exec 3>&-
'@
    }
}

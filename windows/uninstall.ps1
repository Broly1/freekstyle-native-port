<#
.SYNOPSIS
Remove Freekstyle from Windows, without touching anything else.

.DESCRIPTION
The Windows counterpart of linux-uninstall.py, run by double-clicking
windows-uninstall (windows-uninstall.cmd). It lists what it finds and asks
about each part:
  - the installed game (C:\Program Files\Freekstyle, its Start menu shortcut
    and its Installed apps entry)                                [default: yes]
  - build output in the source folder (target\, the generated game code,
    and the files windows-install recorded creating; never your disc dump)
                                                                 [default: yes]
  - the HD textures made while playing (%APPDATA%\freekstyle\textures;
    they are made again as you play)                             [default: yes]
  - your saves and settings (%APPDATA%\freekstyle)               [default: no]

The tools windows-install added with winget and Rust stay installed, as
other software (or a later rebuild) may use them: at the end it lists those
still there, with the command that removes each.

What windows-install did is read from %LOCALAPPDATA%\freekstyle\install.json.
Only files listed there or known to belong to the game are removed.
-DryRun shows what would happen and changes nothing.

windows-install copies it into the installed game's folder, and Settings > Apps
> Installed apps > Freekstyle > Uninstall runs that copy, so it also works
after the source folder is gone. It asks for administrator rights only to
remove the installed game.

.PARAMETER Yes
Don't ask: take each part's default (with -All: everything).
.PARAMETER All
Default to removing everything, your saves and settings included.
.PARAMETER DryRun
Show what would be removed and change nothing.
#>
[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$All,
    [switch]$DryRun,
    # Internal: the administrator half (removing the installed game).
    [switch]$ElevatedRemove,
    # Internal: the folder this was started from, when it re-runs itself
    # from a temporary copy (a running script can't delete its own folder).
    [string]$StartedFrom
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

# The source folder when run from its windows\ folder; the installed game's
# folder when run from there.
$Started = if ($StartedFrom) { $StartedFrom } else { $PSScriptRoot }
$Here = if ((Split-Path -Leaf $Started) -eq 'windows') { Split-Path -Parent $Started } else { $Started }
$Prefix = Join-Path $env:ProgramFiles 'Freekstyle'
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Freekstyle'
$Shortcut = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'Freekstyle.lnk'
$StateFile = Join-Path $env:LOCALAPPDATA 'freekstyle\install.json'
$Data = Join-Path $env:APPDATA 'freekstyle'
$Cargo = Join-Path $env:USERPROFILE '.cargo'

# ---- Messages ----

function Get-Language {
    $codes = @{ pt = 1; es = 2; fr = 3 }
    foreach ($v in 'LANGUAGE', 'LC_ALL', 'LC_MESSAGES', 'LANG') {
        $value = [Environment]::GetEnvironmentVariable($v)
        if ($value -and $value.Length -ge 2) {
            $n = $codes[$value.Substring(0, 2)]
            if ($null -ne $n) { return $n } else { return 0 }
        }
    }
    $n = $codes[(Get-UICulture).TwoLetterISOLanguageName]
    if ($null -ne $n) { return $n }
    return 0
}
$Lang = Get-Language

$Messages = @{
    yes_no = @('[Y/n]', '[S/n]', '[S/n]', '[O/n]')
    yes_no_no = @('[y/N]', '[s/N]', '[s/N]', '[o/N]')
    error = @('error:', 'erro:', 'error:', 'erreur :')
    title = @('Uninstall Freekstyle', 'Desinstalar o Freekstyle', 'Desinstalar Freekstyle', 'Désinstaller Freekstyle')
    nothing = @('Nothing of Freekstyle was found to remove.', 'Nada do Freekstyle foi encontrado para remover.',
                'No se encontró nada de Freekstyle para quitar.', "Rien de Freekstyle n'a été trouvé à supprimer.")
    no_state = @("(no record from windows-install was found, so only the game's own files are offered)",
                 '(nenhum registro do windows-install foi encontrado, então só os arquivos do próprio jogo são oferecidos)',
                 '(no se encontró registro de windows-install, así que solo se ofrecen los archivos del propio juego)',
                 "(aucun relevé de windows-install trouvé, seuls les fichiers du jeu lui-même sont proposés)")
    system = @('The installed game: {0}, its Start menu shortcut and its Installed apps entry (asks for administrator)',
               'O jogo instalado: {0}, o atalho no menu Iniciar e o item em Aplicativos instalados (pede administrador)',
               'El juego instalado: {0}, el acceso directo del menú Inicio y la entrada en Aplicaciones instaladas (pide administrador)',
               "Le jeu installé : {0}, son raccourci du menu Démarrer et son entrée dans Applications installées (demande l'administrateur)")
    build = @('Build output in {0} (the compiled game, generated code, extracted and converted files; your disc dump stays)',
              'Arquivos de compilação em {0} (o jogo compilado, código gerado, arquivos extraídos e convertidos; sua imagem do disco fica)',
              'Archivos de compilación en {0} (el juego compilado, código generado, archivos extraídos y convertidos; tu imagen del disco se queda)',
              'Fichiers de compilation dans {0} (le jeu compilé, le code généré, les fichiers extraits et convertis ; votre image disque reste)')
    hd = @('HD textures made while playing: {0} (made again as you play)', 'Texturas em HD feitas enquanto você jogava: {0} (feitas de novo enquanto joga)',
           'Texturas en HD hechas mientras jugabas: {0} (se hacen de nuevo mientras juegas)', 'Textures HD faites pendant vos parties : {0} (refaites en jouant)')
    data = @('Your saves and settings: {0}', 'Seus saves e configurações: {0}', 'Tus partidas guardadas y ajustes: {0}', 'Vos sauvegardes et réglages : {0}')
    left = @("Left installed (windows-install added them to build the game; other software may use them too).`nTo remove them yourself:",
             "Continuam instalados (o windows-install os adicionou para compilar o jogo; outros programas podem usá-los).`nPara removê-los você mesmo:",
             "Siguen instalados (windows-install los añadió para compilar el juego; otros programas pueden usarlos).`nPara quitarlos tú mismo:",
             "Restent installés (windows-install les a ajoutés pour compiler le jeu ; d'autres logiciels peuvent s'en servir).`nPour les supprimer vous-même :")
    remove_q = @('Remove?', 'Remover?', '¿Quitar?', 'Supprimer ?')
    removing = @('Removing', 'Removendo', 'Quitando', 'Suppression')
    elevate_failed = @('the installed game was not removed (administrator prompt declined)', 'o jogo instalado não foi removido (pedido de administrador recusado)',
                       'el juego instalado no se quitó (solicitud de administrador rechazada)', "le jeu installé n'a pas été supprimé (demande d'administrateur refusée)")
    dry = @('dry run: nothing was changed.', 'simulação: nada foi alterado.', 'simulación: no se cambió nada.', "simulation : rien n'a été modifié.")
    done = @('Done. Freekstyle has been removed.', 'Pronto. O Freekstyle foi removido.', 'Listo. Freekstyle se ha quitado.', 'Terminé. Freekstyle a été supprimé.')
    done_partial = @('Done. Kept: {0}', 'Pronto. Mantido: {0}', 'Listo. Se conserva: {0}', 'Terminé. Conservé : {0}')
    failed = @('this command failed (exit {0}): {1}', 'este comando falhou (saída {0}): {1}', 'este comando falló (salida {0}): {1}',
               'cette commande a échoué (code {0}) : {1}')
    close = @('Press Enter to close', 'Aperte Enter para fechar', 'Pulsa Intro para cerrar', 'Appuyez sur Entrée pour fermer')
}

function T([string]$Key) {
    $text = $Messages[$Key][$Lang]
    if ($args.Count) { return $text -f $args }
    return $text
}

# ---- Helpers ----

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-Size([object[]]$Paths) {
    $total = 0
    foreach ($p in $Paths) {
        if (-not (Test-Path -LiteralPath $p)) { continue }
        $item = Get-Item -LiteralPath $p -Force
        if ($item.PSIsContainer) {
            $total += (Get-ChildItem -LiteralPath $p -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
        } else {
            $total += $item.Length
        }
    }
    return [long]$total
}

function Format-Size([long]$N) {
    foreach ($unit in 'B', 'KB', 'MB', 'GB') {
        if ($N -lt 1024 -or $unit -eq 'GB') {
            if ($unit -eq 'B') { return "$N B" }
            return ('{0:0.0} {1}' -f $N, $unit)
        }
        $N = $N / 1024
    }
}

function Read-State {
    $state = @{}
    if (Test-Path -LiteralPath $StateFile) {
        try {
            (Get-Content -LiteralPath $StateFile -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $state[$_.Name] = $_.Value }
        } catch { }
    }
    return $state
}

# A Freekstyle source folder (where windows-install builds).
function Test-Source([string]$Folder) {
    return $Folder -and (Test-Path -LiteralPath (Join-Path $Folder 'Cargo.toml')) -and (Test-Path -LiteralPath (Join-Path $Folder 'freekstyle')) -and
           (Test-Path -LiteralPath (Join-Path $Folder 'windows\install.ps1'))
}

function Test-Inside([string]$Path, [string]$Folder) {
    $full = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    $base = [IO.Path]::GetFullPath($Folder).TrimEnd('\')
    return $full.StartsWith($base + '\', [StringComparison]::OrdinalIgnoreCase)
}

function Get-SystemPaths {
    $paths = @()
    if (Test-Path -LiteralPath (Join-Path $Prefix 'freekstyle-launcher.exe')) { $paths += $Prefix }
    if (Test-Path -LiteralPath $Shortcut) { $paths += $Shortcut }
    return $paths
}

# Build output in the source folder: target\ and the generated code (pure
# build output), plus the files windows-install recorded creating. The disc
# dump is never among them (game\Freekstyle.bin is only recorded when
# windows-install made it as a link to, or copy of, your dump).
function Get-BuildPaths($State) {
    $source = if ($State.source) { $State.source } else { $Here }
    if (-not (Test-Source $source)) { return $source, @() }
    $paths = @((Join-Path $source 'target'), (Join-Path $source 'freekstyle\src\gen'), (Join-Path $source 'install.log'))
    $paths += @($State.files | Where-Object { $_ } | ForEach-Object { Join-Path $source $_ })
    $out = @($paths | Where-Object { (Test-Path -LiteralPath $_) -and (Test-Inside $_ $source) } | Select-Object -Unique)
    # Cargo's intermediate files, kept outside the source folder by
    # install.ps1 (CARGO_BUILD_BUILD_DIR).
    $buildDir = Join-Path $env:LOCALAPPDATA 'freekstyle\build'
    if (Test-Path -LiteralPath $buildDir) { $out += $buildDir }
    return $source, $out
}

# The texture pack the game builds while playing, and what goes with it
# (freek_config::data_dir in the game).
function Get-HdPaths {
    $paths = @('textures', 'hd.log', 'hd-building.lock') | ForEach-Object { Join-Path $Data $_ }
    return @($paths | Where-Object { Test-Path -LiteralPath $_ })
}

function Get-DataPaths($State) {
    $paths = @($Data)
    $source = if ($State.source) { $State.source } else { $Here }
    if (Test-Source $source) { $paths += Join-Path $source 'freekstyle.toml' }
    return @($paths | Where-Object { Test-Path -LiteralPath $_ })
}

function Test-WingetInstalled([string]$Id) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) { return $false }
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & winget list --exact --id $Id --accept-source-agreements 2>&1 | Out-Null
        return $LASTEXITCODE -eq 0
    } finally { $ErrorActionPreference = $saved }
}

function Show-Run([object[]]$Cmd) {
    Write-Host ('  > ' + ($Cmd -join ' ')) -ForegroundColor DarkGray
    if ($DryRun) { return $true }
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $rest = @()
        if ($Cmd.Count -gt 1) { $rest = @($Cmd[1..($Cmd.Count - 1)]) }
        & $Cmd[0] @rest | Out-Host   # shown, not taken as this function's result
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $saved }
    if ($code -ne 0) {
        Write-Host '  ' -NoNewline
        Write-Host (T 'error') -ForegroundColor Red -NoNewline
        Write-Host (' ' + (T 'failed' $code ($Cmd -join ' ')))
    }
    return $code -eq 0
}

function Remove-Paths([object[]]$Paths) {
    $n = 0
    foreach ($p in $Paths) {
        if ($n -lt 4 -or $Paths.Count -le 5) {
            Write-Host ("  del $p".Replace($env:USERPROFILE, '~')) -ForegroundColor DarkGray
        } elseif ($n -eq 4) {
            Write-Host ('  ... (+{0})' -f ($Paths.Count - 4)) -ForegroundColor DarkGray
        }
        $n++
        if ($DryRun) { continue }
        Remove-Item -LiteralPath $p -Recurse -Force -ErrorAction Continue
    }
}

# ---- Removing ----

# The administrator half: the installed game, its shortcut and its
# Installed apps entry.
function Remove-SystemInstall {
    Remove-Paths (Get-SystemPaths)
    if (-not $DryRun -and (Test-Path -LiteralPath $UninstallKey)) { Remove-Item -LiteralPath $UninstallKey -Recurse -Force }
}

function Remove-System {
    if ((Test-Admin) -or $DryRun) {
        Remove-SystemInstall
        return
    }
    # The administrator half runs the installed copy, which only
    # administrators can change, never the temporary copy this may be
    # running from: another program could swap that one before UAC is
    # answered and have it run as administrator.
    $script = Join-Path $Prefix 'uninstall.ps1'
    if (-not (Test-Path -LiteralPath $script)) { $script = $PSCommandPath }
    $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$script`"", '-ElevatedRemove')
    try {
        $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argv -Wait -PassThru -WindowStyle Hidden
        $ok = $p.ExitCode -eq 0
    } catch { $ok = $false }
    if ($ok) {
        Get-SystemPaths | ForEach-Object { Write-Host "  del $_" -ForegroundColor DarkGray }
        Write-Host "  del $Prefix" -ForegroundColor DarkGray
    } else {
        Write-Host '  ' -NoNewline
        Write-Host (T 'error') -ForegroundColor Red -NoNewline
        Write-Host (' ' + (T 'elevate_failed'))
    }
}

function Remove-Build($State) {
    $source, $paths = Get-BuildPaths $State
    Remove-Paths $paths
    # Folders the converted files were in, if nothing else is left there.
    foreach ($folder in (Join-Path $source 'game\movies\hd'), (Join-Path $source 'game\movies')) {
        if (-not $DryRun -and (Test-Path -LiteralPath $folder) -and -not (Get-ChildItem -LiteralPath $folder -Force)) { Remove-Item -LiteralPath $folder }
    }
}

# What windows-install added that is still installed, with the command
# that removes each: shown at the end, never removed here.
function Show-Leftovers($State) {
    $left = @()
    foreach ($id in @($State.winget | Where-Object { $_ })) {
        if (Test-WingetInstalled $id) { $left += , @($id, "winget uninstall --exact --id $id") }
    }
    if ($State.rustup -and (Test-Path -LiteralPath (Join-Path $Cargo 'bin\rustup.exe'))) {
        $left += , @('Rust (rustup, .cargo .rustup)', 'rustup self uninstall')
    }
    if (-not $left.Count) { return }
    Write-Host ''
    Write-Host (T 'left')
    foreach ($item in $left) {
        Write-Host ('  - ' + $item[0])
        Write-Host ('      ' + $item[1]) -ForegroundColor White
    }
}

# Forget what was removed, so a later run doesn't offer it again. Once the
# game and its build output are gone the record goes too (the list of what
# was left installed has just been shown).
function Save-State($State, [string[]]$Chosen) {
    if ($DryRun -or $State.Count -eq 0) { return }
    if ($Chosen -contains 'build') { $State['files'] = @() }
    if ($Chosen -contains 'system') { $State.Remove('system') }
    if (-not ($State.system -or @($State.files | Where-Object { $_ }).Count)) {
        Remove-Item -LiteralPath $StateFile -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Split-Path $StateFile) -ErrorAction SilentlyContinue   # only if empty
    } else {
        $State | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $StateFile -Encoding UTF8
    }
}

function Ask([string]$Question, [bool]$Default) {
    $Default = $Default -or $All
    $hint = if ($Default) { T 'yes_no' } else { T 'yes_no_no' }
    if ($Yes -or $DryRun) {
        Write-Host ('  {0} {1} {2}' -f $Question, $hint, $(if ($Default) { 'y' } else { 'n' }))
        return $Default
    }
    Write-Host ('  {0} {1} ' -f $Question, $hint) -NoNewline
    $reply = (Read-Host).Trim().ToLower()
    if (-not $reply) { return $Default }
    return 'yso'.IndexOf($reply[0]) -ge 0   # yes, sim/sí, oui
}

function Main {
    if ($ElevatedRemove) {
        try { Remove-SystemInstall; exit 0 } catch { exit 1 }
    }
    Write-Host (T 'title') -ForegroundColor White
    $state = Read-State
    if ($state.Count -eq 0) { Write-Host (T 'no_state') -ForegroundColor DarkGray }

    $parts = @()
    $system = @(Get-SystemPaths)
    if ($system.Count -or (Test-Path -LiteralPath $UninstallKey)) {
        $parts += [pscustomobject]@{ Key = 'system'; Label = (T 'system' $Prefix); Default = $true; Size = (Get-Size $system); Note = $null }
    }
    $source, $build = Get-BuildPaths $state
    if ($build.Count) {
        $parts += [pscustomobject]@{ Key = 'build'; Label = (T 'build' $source); Default = $true; Size = (Get-Size $build); Note = $null }
    }
    $hd = @(Get-HdPaths)
    if ($hd.Count) {
        $parts += [pscustomobject]@{ Key = 'hd'; Label = (T 'hd' (Join-Path $Data 'textures').Replace($env:USERPROFILE, '~')); Default = $true
                                     Size = (Get-Size $hd); Note = $null }
    }
    # Not $data: PowerShell names ignore case, and the functions called
    # from here would see it in place of the script's $Data folder.
    $saves = @(Get-DataPaths $state)
    if ($saves.Count) {
        $parts += [pscustomobject]@{ Key = 'data'; Label = (T 'data' (($saves -join ', ').Replace($env:USERPROFILE, '~'))); Default = $false
                                     Size = (Get-Size $saves); Note = $null }
    }
    if ($parts.Count -eq 0) {
        Write-Host (T 'nothing')
        Show-Leftovers $state
        return 0
    }

    $chosen = @()
    $kept = @()
    foreach ($part in $parts) {
        Write-Host ''
        Write-Host ('- ' + $part.Label) -NoNewline
        if ($part.Size) { Write-Host ('  ({0})' -f (Format-Size $part.Size)) -ForegroundColor DarkGray } else { Write-Host '' }
        if ($part.Note) { Write-Host ('  ' + $part.Note) -ForegroundColor DarkGray }
        if (Ask (T 'remove_q') $part.Default) { $chosen += $part } else { $kept += $part }
    }
    if ($chosen.Count) {
        Write-Host ''
        Write-Host (T 'removing') -ForegroundColor White
    }
    foreach ($part in $chosen) {
        switch ($part.Key) {
            'system' { Remove-System }
            'build' { Remove-Build $state }
            'hd' { Remove-Paths (Get-HdPaths) }
            'data' { Remove-Paths (Get-DataPaths $state) }
        }
    }
    Save-State $state @($chosen | ForEach-Object Key)
    Write-Host ''
    if ($DryRun) {
        Write-Host (T 'dry')
    } elseif ($kept.Count) {
        Write-Host (T 'done_partial' (($kept | ForEach-Object { ($_.Label -split '[:(]')[0].Trim() }) -join '; '))
    } else {
        Write-Host (T 'done') -ForegroundColor Green
    }
    Show-Leftovers $state
    return 0
}

# Started from the installed game's folder (Installed apps runs it from
# there): carry on from a temporary copy, so that folder can go. Run at the
# top level, not inside Main, so the copy's output reaches the window
# instead of being taken as Main's result.
if (-not $ElevatedRemove -and -not $StartedFrom -and (Test-Inside $PSCommandPath $Prefix)) {
    # A new folder with a random name, so nothing can be waiting there. Run
    # as administrator (as Installed apps does), only administrators may
    # write in it, so no other program can change the copy before it runs.
    $dir = Join-Path $env:TEMP ('freekstyle-uninstall-' + [IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $dir | Out-Null
    if (Test-Admin) {
        & icacls.exe $dir /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "could not protect $dir" }
    }
    $tmp = Join-Path $dir 'uninstall.ps1'
    Copy-Item -LiteralPath $PSCommandPath $tmp
    # Out of the folder too: a process's working directory keeps it in
    # use, and this one waits on the copy. The copy starts in the same.
    Set-Location -LiteralPath $env:TEMP
    [IO.Directory]::SetCurrentDirectory($env:TEMP)
    $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $tmp, '-StartedFrom', $PSScriptRoot)
    if ($Yes) { $argv += '-Yes' }
    if ($All) { $argv += '-All' }
    if ($DryRun) { $argv += '-DryRun' }
    & powershell.exe @argv
    $code = $LASTEXITCODE
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    exit $code
}

$code = Main
# Installed apps opens a window for this; leave it up until it's read.
if (-not $Yes -and -not $DryRun -and $Host.Name -eq 'ConsoleHost') {
    Write-Host ''
    [void](Read-Host (T 'close'))
}
exit $code

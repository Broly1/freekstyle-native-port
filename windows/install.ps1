<#
.SYNOPSIS
Set up Freekstyle on Windows from your own disc dump.

.DESCRIPTION
The Windows counterpart of linux-install.py. Put the dump (.bin or .iso) in game\
and double-click windows-install (windows-install.cmd, which runs this). It installs what is missing (asking
first, with winget), copies what the port needs off the disc, generates the
game code and builds it, converts the pictures and movies, and offers to
install the game for all users (C:\Program Files\Freekstyle, with Freekstyle
in the Start menu and in Settings > Apps > Installed apps, where it can be
uninstalled). Steps already done are skipped, so it is safe to run again
after a `git pull`. What it adds (winget packages, Rust, files, the
installed game) is recorded in %LOCALAPPDATA%\freekstyle\install.json, so
that windows\uninstall.ps1 can remove exactly that and nothing else.

It speaks English, Brazilian Portuguese, Spanish or French, following the
Windows display language (or LANG, as on Linux). Everything it runs is
logged to install.log. It needs nothing but the PowerShell that comes with
Windows. Run it as your own user: it asks for administrator rights only to
copy the game into Program Files.

.PARAMETER Yes
Answer yes to every question (installs tools and Rust without asking).
.PARAMETER System
Install to Program Files at the end without asking.
.PARAMETER Here
Keep the game in this folder at the end without asking.
.PARAMETER Uninstall
Run windows\uninstall.ps1 (removes the game and what this script added).
#>
[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$System,
    [switch]$Here,
    [switch]$Uninstall,
    # Internal: the administrator half of the install step (the copy into
    # Program Files), started by this script through a UAC prompt.
    [string]$ElevatedInstall
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest crawls with its progress bar
[Console]::OutputEncoding = [Text.Encoding]::UTF8

# This script lives in windows\ in the source folder.
$Root = Split-Path -Parent $PSScriptRoot
$Game = Join-Path $Root 'game'
$Log = Join-Path $Root 'install.log'
$SlusSha256 = 'debcb4b0731741a389a0c890296609eb99d111952e97a250ea4c3cfc04a6d80a'
$MovieList = @('OPENING/BIGLOGO', 'OPENING/EAGAMES', 'OPENING/EAS_BIG', 'OPENING/INTRO', 'ABOUTEA/PROD1')
$Prefix = Join-Path $env:ProgramFiles 'Freekstyle'
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Freekstyle'
# Free space wanted for the build (it ends up around 7 GB) and the copies.
$SpaceGB = 10
# What this script added, for uninstall.ps1: kept outside the source folder
# so it survives that folder being deleted.
$StateFile = Join-Path $env:LOCALAPPDATA 'freekstyle\install.json'

# Tools winget can install: requirement key -> winget id and extra arguments.
$WingetPackages = @{
    compiler = @{ Id = 'Microsoft.VisualStudio.2022.BuildTools'
                  Args = @('--override', '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended') }
    cmake    = @{ Id = 'Kitware.CMake'; Args = @() }
}
# ffmpeg isn't installed with winget: a per-user winget package can't be
# removed by an uninstaller running as administrator, which is how
# Installed apps runs it. A build goes next to the game instead, where the
# game finds it, and leaves with the build output.
$FfmpegUrl = 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip'
$Release = Join-Path $Root 'target\release'

# ---- Messages ----

function Get-Language {
    # 0 English, 1 Portuguese (Brazil), 2 Spanish, 3 French: LANG and friends
    # first, as in the launcher (launcher/src/i18n.rs), then Windows' own.
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

# key: (English, Portuguese (Brazil), Spanish, French); {0} for arguments.
$Messages = @{
    yes_no = @('[Y/n]', '[S/n]', '[S/n]', '[O/n]')
    yes_no_no = @('[y/N]', '[s/N]', '[s/N]', '[o/N]')
    error = @('error:', 'erro:', 'error:', 'erreur :')
    warning = @('warning:', 'aviso:', 'aviso:', 'attention :')
    admin = @('this is running as administrator. Run it as your own user instead (double-click windows-install): it asks for administrator rights when it needs them, and Rust and the build then belong to you.',
              'isto está rodando como administrador. Rode como seu usuário (clique duas vezes em windows-install): ele pede direitos de administrador quando precisa, e a Rust e a compilação ficam sendo suas.',
              'esto se está ejecutando como administrador. Ejecútalo como tu usuario (doble clic en windows-install): pide derechos de administrador cuando los necesita, y Rust y la compilación quedan a tu nombre.',
              "ceci tourne en administrateur. Lancez-le plutôt avec votre utilisateur (double-clic sur windows-install) : il demande les droits d'administrateur quand il en a besoin, et Rust et la compilation vous appartiennent alors.")
    look_dump = @('Your disc dump', 'Imagem do disco', 'Imagen del disco', 'Image du disque')
    no_dump = @('no disc image in game\. Dump your Freekstyle disc (NTSC-U, SLUS-20367) as a .bin or .iso into game\, then run this again.',
                'nenhuma imagem de disco em game\. Copie seu disco do Freekstyle (NTSC-U, SLUS-20367) como .bin ou .iso para game\ e rode de novo.',
                'no hay imagen de disco en game\. Copia tu disco de Freekstyle (NTSC-U, SLUS-20367) como .bin o .iso en game\ y vuelve a ejecutarlo.',
                'aucune image disque dans game\. Copiez votre disque Freekstyle (NTSC-U, SLUS-20367) en .bin ou .iso dans game\, puis relancez.')
    using_dump = @('using {0} (linked as game\Freekstyle.bin)', 'usando {0} (ligado como game\Freekstyle.bin)',
                   'usando {0} (enlazado como game\Freekstyle.bin)', 'utilisation de {0} (lié en game\Freekstyle.bin)')
    many_dumps = @('several disc images in game\ ({0}); rename yours to Freekstyle.bin.',
                   'várias imagens de disco em game\ ({0}); renomeie a sua para Freekstyle.bin.',
                   'varias imágenes de disco en game\ ({0}); renombra la tuya a Freekstyle.bin.',
                   'plusieurs images disque dans game\ ({0}) ; renommez la vôtre en Freekstyle.bin.')
    found_dump = @('found game\Freekstyle.bin', 'encontrado game\Freekstyle.bin', 'encontrado game\Freekstyle.bin', 'trouvé game\Freekstyle.bin')
    space = @('only {0} GB free here; the build needs about {1} GB.', 'só {0} GB livres aqui; a compilação precisa de uns {1} GB.',
              'solo {0} GB libres aquí; la compilación necesita unos {1} GB.', 'seulement {0} Go libres ici ; la compilation demande environ {1} Go.')
    continue = @('Continue anyway?', 'Continuar mesmo assim?', '¿Continuar de todos modos?', 'Continuer quand même ?')
    check_tools = @('Tools', 'Ferramentas', 'Herramientas', 'Outils')
    req_compiler = @('Visual Studio C++ Build Tools (compiler, linker, Windows SDK)', 'Visual Studio C++ Build Tools (compilador, linker, SDK do Windows)',
                     'Visual Studio C++ Build Tools (compilador, enlazador, SDK de Windows)', "Visual Studio C++ Build Tools (compilateur, éditeur de liens, SDK Windows)")
    req_cmake = @('CMake (builds SDL3, for controllers)', 'CMake (compila o SDL3, para os controles)', 'CMake (compila SDL3, para los mandos)', 'CMake (compile SDL3, pour les manettes)')
    req_vulkan = @('Vulkan (graphics; comes with your graphics driver)', 'Vulkan (gráficos; vem com o driver de vídeo)',
                   'Vulkan (gráficos; viene con el controlador gráfico)', 'Vulkan (graphismes ; fourni avec le pilote graphique)')
    no_vulkan = @("no Vulkan found. The game needs it to start: install or update your graphics card's driver (AMD, Intel or NVIDIA).",
                  'Vulkan não encontrado. O jogo precisa dele para abrir: instale ou atualize o driver da placa de vídeo (AMD, Intel ou NVIDIA).',
                  'no se encontró Vulkan. El juego lo necesita para arrancar: instala o actualiza el controlador de tu tarjeta gráfica (AMD, Intel o NVIDIA).',
                  'Vulkan introuvable. Le jeu en a besoin pour démarrer : installez ou mettez à jour le pilote de votre carte graphique (AMD, Intel ou NVIDIA).')
    req_ffmpeg = @('ffmpeg (movies, optional)', 'ffmpeg (vídeos, opcional)', 'ffmpeg (vídeos, opcional)', 'ffmpeg (vidéos, facultatif)')
    get_ffmpeg = @('Download ffmpeg for the movies (a build from github.com/BtbN/FFmpeg-Builds, about 200 MB)? It goes next to the game in target\release and leaves with the build.',
                   'Baixar o ffmpeg para os vídeos (uma versão de github.com/BtbN/FFmpeg-Builds, uns 200 MB)? Ele fica ao lado do jogo em target\release e sai junto com a compilação.',
                   '¿Descargar ffmpeg para los vídeos (una versión de github.com/BtbN/FFmpeg-Builds, unos 200 MB)? Va junto al juego en target\release y se va con la compilación.',
                   "Télécharger ffmpeg pour les vidéos (une version de github.com/BtbN/FFmpeg-Builds, environ 200 Mo) ? Il va à côté du jeu dans target\release et part avec la compilation.")
    ffmpeg_failed = @('could not download ffmpeg ({0}); the game skips its movies.', 'não foi possível baixar o ffmpeg ({0}); o jogo pula os vídeos.',
                      'no se pudo descargar ffmpeg ({0}); el juego omite sus vídeos.', "impossible de télécharger ffmpeg ({0}) ; le jeu saute ses vidéos.")
    no_winget = @('missing: {0}. winget (App Installer, from the Microsoft Store) is needed to install them; or install them yourself and run this again.',
                  'faltando: {0}. É preciso o winget (Instalador de Aplicativo, da Microsoft Store) para instalá-los; ou instale-os você mesmo e rode de novo.',
                  'faltan: {0}. Hace falta winget (Instalador de aplicación, de Microsoft Store) para instalarlos; o instálalos tú mismo y vuelve a ejecutarlo.',
                  "manquant : {0}. winget (Programme d'installation d'application, depuis le Microsoft Store) est nécessaire pour les installer ; ou installez-les vous-même et relancez.")
    install_now = @("Install what's missing now with winget?`n  {0}", 'Instalar agora o que falta com o winget?`n  {0}',
                    '¿Instalar ahora lo que falta con winget?`n  {0}', 'Installer maintenant ce qui manque avec winget ?`n  {0}')
    vs_note = @('(the Build Tools are a download of a few GB and take a while)', '(as Build Tools são um download de alguns GB e demoram um pouco)',
                '(las Build Tools son una descarga de unos GB y tardan un rato)', '(les Build Tools pèsent quelques Go à télécharger et prennent un moment)')
    cant_build = @("can't build without them.", 'não dá para compilar sem eles.', 'no se puede compilar sin ellos.', 'impossible de compiler sans eux.')
    still_missing = @('still missing after installing: {0}', 'ainda faltando depois de instalar: {0}',
                      'todavía faltan después de instalar: {0}', "toujours manquant après l'installation : {0}")
    rust = @('Rust', 'Rust', 'Rust', 'Rust')
    rust_old = @('Rust {0} is too old: the port needs {1} or newer.', 'Rust {0} é antiga demais: o port precisa da {1} ou mais nova.',
                 'Rust {0} es demasiado antiguo: el port necesita {1} o más reciente.', 'Rust {0} est trop ancien : le portage demande {1} ou plus récent.')
    rust_none = @('Rust is not installed.', 'Rust não está instalada.', 'Rust no está instalado.', "Rust n'est pas installé.")
    rustup_update = @('Update it with rustup (rustup default stable, rustup update stable)?',
                      'Atualizar com rustup (rustup default stable, rustup update stable)?',
                      '¿Actualizarlo con rustup (rustup default stable, rustup update stable)?',
                      'Le mettre à jour avec rustup (rustup default stable, rustup update stable) ?')
    rustup_install = @('Install current Rust with rustup (https://rustup.rs, into your user folder, .cargo and .rustup)?',
                       'Instalar a Rust atual com rustup (https://rustup.rs, na sua pasta de usuário, .cargo e .rustup)?',
                       '¿Instalar Rust actual con rustup (https://rustup.rs, en tu carpeta de usuario, .cargo y .rustup)?',
                       'Installer Rust à jour avec rustup (https://rustup.rs, dans votre dossier utilisateur, .cargo et .rustup) ?')
    rust_needed = @('Rust {0} or newer is needed.', 'É preciso Rust {0} ou mais nova.', 'Hace falta Rust {0} o más reciente.', 'Rust {0} ou plus récent est nécessaire.')
    rust_still = @('still no Rust {0} or newer (found: {1}).', 'ainda sem Rust {0} ou mais nova (encontrada: {1}).',
                   'todavía no hay Rust {0} o más reciente (encontrado: {1}).', 'toujours pas de Rust {0} ou plus récent (trouvé : {1}).')
    copy_prog = @("The game's program", 'O programa do jogo', 'El programa del juego', 'Le programme du jeu')
    bad_slus = @('game\SLUS_203.67 is not the NTSC-U release this port needs (SLUS-20367). Delete it and use a dump of that disc.',
                 'game\SLUS_203.67 não é a versão NTSC-U de que o port precisa (SLUS-20367). Apague-o e use uma cópia desse disco.',
                 'game\SLUS_203.67 no es la versión NTSC-U que necesita el port (SLUS-20367). Bórralo y usa una copia de ese disco.',
                 "game\SLUS_203.67 n'est pas la version NTSC-U dont le portage a besoin (SLUS-20367). Supprimez-le et utilisez une copie de ce disque.")
    building = @('Generating the game code and building (a few minutes the first time)', 'Gerando o código do jogo e compilando (alguns minutos na primeira vez)',
                 'Generando el código del juego y compilando (unos minutos la primera vez)', 'Génération du code du jeu et compilation (quelques minutes la première fois)')
    pictures = @('Title and menu pictures', 'Imagens de título e menu', 'Imágenes de título y menú', 'Images de titre et de menu')
    already = @('already done', 'já feito', 'ya hecho', 'déjà fait')
    movies = @('Movies', 'Vídeos', 'Vídeos', 'Vidéos')
    no_ffmpeg = @('skipped: no ffmpeg (the game skips its movies)', 'pulado: sem ffmpeg (o jogo pula os vídeos)',
                  'omitido: no hay ffmpeg (el juego omite sus vídeos)', 'ignoré : pas de ffmpeg (le jeu saute ses vidéos)')
    no_mpeg2 = @("skipped: this ffmpeg can't decode the movies (MPEG-2), so the game skips them. Install a full ffmpeg and run this again.",
                 'pulado: este ffmpeg não decodifica os vídeos (MPEG-2), então o jogo os pula. Instale um ffmpeg completo e rode de novo.',
                 'omitido: este ffmpeg no decodifica los vídeos (MPEG-2), así que el juego los omite. Instala un ffmpeg completo y vuelve a ejecutarlo.',
                 'ignoré : ce ffmpeg ne décode pas les vidéos (MPEG-2), le jeu les saute donc. Installez un ffmpeg complet, puis relancez.')
    movie_audio = @('turned down {0} movie soundtrack(s) that went over full volume (they crackled)',
                    '{0} trilha(s) de vídeo que passavam do volume máximo (estalavam) foram abaixadas',
                    'se bajó el volumen de {0} banda(s) sonora(s) de vídeo que superaban el máximo (crujían)',
                    '{0} bande(s) son de vidéo qui dépassaient le volume maximal (grésillaient) ont été baissées')
    crt = @('with the CRT look', 'com o visual CRT', 'con aspecto CRT', 'avec le rendu CRT')
    no_crt = @('CRT look unavailable (needs ffmpeg with libplacebo and a GPU); converting the movies without it',
               'visual CRT indisponível (precisa de ffmpeg com libplacebo e uma GPU); convertendo os vídeos sem ele',
               'aspecto CRT no disponible (necesita ffmpeg con libplacebo y una GPU); convirtiendo los vídeos sin él',
               'rendu CRT indisponible (il faut ffmpeg avec libplacebo et un GPU) ; conversion des vidéos sans lui')
    movies_failed = @('some movies could not be converted (see install.log); the game skips those.',
                      'alguns vídeos não puderam ser convertidos (veja install.log); o jogo os pula.',
                      'algunos vídeos no se pudieron convertir (ver install.log); el juego los omite.',
                      "certaines vidéos n'ont pas pu être converties (voir install.log) ; le jeu les saute.")
    installing = @('Installing', 'Instalando', 'Instalando', 'Installation')
    opt_system = @('  1) Install for all users: {0}, in the Start menu and in Installed apps; asks for administrator',
                   '  1) Instalar para todos os usuários: {0}, no menu Iniciar e em Aplicativos instalados; pede administrador',
                   '  1) Instalar para todos los usuarios: {0}, en el menú Inicio y en Aplicaciones instaladas; pide administrador',
                   "  1) Installer pour tous les utilisateurs : {0}, dans le menu Démarrer et dans Applications installées ; demande l'administrateur")
    opt_here = @('  2) Keep it in this folder', '  2) Deixar nesta pasta', '  2) Dejarlo en esta carpeta', '  2) Le garder dans ce dossier')
    choose = @('Choose [1]', 'Escolha [1]', 'Elige [1]', 'Choix [1]')
    copying_disc = @('copying the game (the disc image is 660 MB)...', 'copiando o jogo (a imagem do disco tem 660 MB)...',
                     'copiando el juego (la imagen del disco ocupa 660 MB)...', "copie du jeu (l'image du disque fait 660 Mo)...")
    elevate_failed = @('the copy into Program Files did not finish (administrator prompt declined, or see install.log); the game is still playable from this folder.',
                       'a cópia para Arquivos de Programas não terminou (pedido de administrador recusado, ou veja install.log); o jogo continua jogável a partir desta pasta.',
                       'la copia a Archivos de programa no terminó (solicitud de administrador rechazada, o ver install.log); el juego sigue siendo jugable desde esta carpeta.',
                       "la copie dans Program Files n'a pas abouti (demande d'administrateur refusée, ou voir install.log) ; le jeu reste jouable depuis ce dossier.")
    done = @('Done in {0}', 'Pronto em {0}', 'Listo en {0}', 'Terminé en {0}')
    done_system = @("Freekstyle is in the Start menu. This folder isn't needed to play any more;`nSettings > Apps > Installed apps > Freekstyle > Uninstall (or windows-uninstall here)`nremoves it and what this script added.",
                    'O Freekstyle está no menu Iniciar. Esta pasta não é mais necessária para jogar;`nConfigurações > Aplicativos > Aplicativos instalados > Freekstyle > Desinstalar (ou windows-uninstall aqui)`nremove o jogo e o que este script adicionou.',
                    'Freekstyle está en el menú Inicio. Esta carpeta ya no hace falta para jugar;`nConfiguración > Aplicaciones > Aplicaciones instaladas > Freekstyle > Desinstalar (o windows-uninstall aquí)`nquita el juego y lo que añadió este script.',
                    "Freekstyle est dans le menu Démarrer. Ce dossier n'est plus nécessaire pour jouer ;`nParamètres > Applications > Applications installées > Freekstyle > Désinstaller (ou windows-uninstall ici)`nsupprime le jeu et ce que ce script a ajouté.")
    done_here = @('Start the game with:   target\release\freekstyle-launcher.exe', 'Inicie o jogo com:   target\release\freekstyle-launcher.exe',
                  'Inicia el juego con:   target\release\freekstyle-launcher.exe', 'Lancez le jeu avec :   target\release\freekstyle-launcher.exe')
    failed_cmd = @('this command failed (exit {0}): {1}', 'este comando falhou (saída {0}): {1}', 'este comando falló (salida {0}): {1}', 'cette commande a échoué (code {0}) : {1}')
    see_log = @('the full output is in {0}', 'a saída completa está em {0}', 'la salida completa está en {0}', 'la sortie complète est dans {0}')
    interrupted = @('interrupted.', 'interrompido.', 'interrumpido.', 'interrompu.')
    shortcut = @('Freekstyle display settings and controls', 'Vídeo e controles do Freekstyle', 'Pantalla y controles de Freekstyle', 'Affichage et commandes de Freekstyle')
}

function T([string]$Key) {
    $text = $Messages[$Key][$Lang] -replace '`n', "`n"
    if ($args.Count) { return $text -f $args }
    return $text
}

# ---- Output ----

# A problem the user can fix; the message says how. Logged: a command
# failed, and its output is in the log.
class Failure : System.Exception {
    [bool]$Logged
    Failure([string]$Message, [bool]$Logged) : base($Message) { $this.Logged = $Logged }
}

function Write-Log([string]$Text) {
    [IO.File]::AppendAllText($Log, $Text + "`r`n", [Text.Encoding]::UTF8)
}

$script:StepNumber = 0
$StepTotal = 7

function Step([string]$Title) {
    $script:StepNumber++
    $line = '[{0}/{1}] {2}' -f $script:StepNumber, $StepTotal, $Title
    Write-Host ''
    Write-Host $line -ForegroundColor White
    Write-Log "`r`n== $line"
}

function Say([string]$Text, [string]$Mark, [ConsoleColor]$Colour = 'Gray') {
    $Text = $Text -replace "`n", "`n  "
    if ($Mark) {
        Write-Host '  ' -NoNewline
        Write-Host $Mark -ForegroundColor $Colour -NoNewline
        Write-Host " $Text"
    } else {
        Write-Host "  $Text"
    }
    Write-Log ("$Mark $Text".Trim())
}
function Good([string]$Text) { Say $Text ([string][char]0x2713) Green }
function Bad([string]$Text) { Say $Text ([string][char]0x2717) Red }
function Warn([string]$Text) { Say $Text (T 'warning') Yellow }

function Ask([string]$Question, [bool]$Default = $true) {
    $hint = if ($Default) { T 'yes_no' } else { T 'yes_no_no' }
    $prompt = '  {0} {1} ' -f ($Question -replace "`n", "`n  "), $hint
    if ($Yes) {
        Write-Host ($prompt + 'y')
        return $true
    }
    Write-Host $prompt -NoNewline
    $reply = (Read-Host).Trim().ToLower()
    if (-not $reply) { return $Default }
    return 'yso'.IndexOf($reply[0]) -ge 0   # yes, sim/sí, oui
}

function Choose([string]$Prompt, [string]$Default) {
    if ($Yes) {
        Write-Host "  ${Prompt}: $Default"
        return $Default
    }
    $reply = (Read-Host "  $Prompt").Trim()
    if ($reply) { return $reply }
    return $Default
}

function Format-Command([object[]]$Cmd) {
    ($Cmd | ForEach-Object { $s = "$_"; if ($s -match '\s') { '"' + $s + '"' } else { $s } }) -join ' '
}

# Run a program, showing and logging its output (Quiet: only logged).
# Returns the exit code.
function Invoke-Logged([object[]]$Cmd, [switch]$Quiet, [string]$Cwd = $Root) {
    $shown = Format-Command $Cmd
    if (-not $Quiet) { Write-Host "  > $shown" -ForegroundColor DarkGray }
    Write-Log "> $shown"
    $exe = "$($Cmd[0])"
    $rest = @()
    if ($Cmd.Count -gt 1) { $rest = @($Cmd[1..($Cmd.Count - 1)] | ForEach-Object { "$_" }) }
    Push-Location $Cwd
    # Native programs' stderr comes through as error records; they're just
    # output here (cargo reports progress on stderr).
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $global:LASTEXITCODE = 0
        & $exe @rest 2>&1 | ForEach-Object {
            # A blank stderr line shows as the exception's type name.
            $line = if ($_ -is [Management.Automation.ErrorRecord]) { $_.Exception.Message } else { "$_" }
            if ($line -eq 'System.Management.Automation.RemoteException') { $line = '' }
            Write-Log $line
            if (-not $Quiet) { Write-Host "    $line" }
        }
        $code = $LASTEXITCODE
    } catch {
        Write-Log "$_"
        if (-not $Quiet) { Write-Host "    $_" }
        $code = 1
    } finally {
        $ErrorActionPreference = $saved
        Pop-Location
    }
    if ($null -eq $code) { $code = 0 }
    Write-Log "(exit $code)"
    return $code
}

# Run a program; a failure stops the install with a message.
function Run([object[]]$Cmd, [switch]$Quiet, [string]$Cwd = $Root) {
    $code = Invoke-Logged $Cmd -Quiet:$Quiet -Cwd $Cwd
    if ($code -ne 0) { throw [Failure]::new((T 'failed_cmd' $code (Format-Command $Cmd)), $true) }
}

# Run a program that may fail; whether it worked.
function TryRun([object[]]$Cmd, [switch]$Quiet, [string]$Cwd = $Root) {
    return (Invoke-Logged $Cmd -Quiet:$Quiet -Cwd $Cwd) -eq 0
}

function Have([string]$Name) { return [bool](Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue) }

# A program's output (stdout and stderr), or '' if it can't run.
function Output([string]$Exe) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { return ((& $Exe @args 2>&1 | ForEach-Object { "$_" }) -join "`n") } catch { return '' } finally { $ErrorActionPreference = $saved }
}

# ---- The machine ----

# PATH as Windows has it now, with what installers just added, plus Rust's
# and CMake's folders when they exist.
function Update-Path {
    $paths = @([Environment]::GetEnvironmentVariable('Path', 'Machine'), [Environment]::GetEnvironmentVariable('Path', 'User'))
    $env:Path = ($paths | Where-Object { $_ }) -join ';'
    $cargo = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path $cargo) { $env:Path = "$cargo;$env:Path" }
    $cmake = Join-Path $env:ProgramFiles 'CMake\bin'
    if ((Test-Path $cmake) -and -not (Have 'cmake')) { $env:Path = "$env:Path;$cmake" }
    # The ffmpeg this script downloads, next to the game.
    if (Test-Path (Join-Path $Release 'ffmpeg.exe')) { $env:Path = "$Release;$env:Path" }
}

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# MSVC (the compiler and linker Rust uses on Windows) and a Windows SDK.
function Test-Msvc {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }
    $vs = Output $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $sdk = Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Microsoft SDKs\Windows\v10.0' -Name InstallationFolder -ErrorAction SilentlyContinue
    return [bool]($vs.Trim()) -and [bool]$sdk
}

# The game draws with Vulkan; graphics drivers install its loader.
function Test-Vulkan { return Test-Path (Join-Path $env:SystemRoot 'System32\vulkan-1.dll') }

# The movies are MPEG-2, which some ffmpeg builds leave out.
function Test-FfmpegMpeg2 { return (Output ffmpeg -hide_banner -decoders) -match '(?m)^ V.{5} mpeg2video ' }

# Highest sample of a movie's soundtrack in dB (0 is full scale), or $null.
function Get-MoviePeak([string]$Path) {
    $err = Output ffmpeg -hide_banner -nostats -i $Path -vn -af 'aformat=sample_fmts=flt,astats=measure_perchannel=none:measure_overall=Peak_level' -f null -
    $found = [regex]::Matches($err, 'Peak level dB: (-?[0-9.]+|-inf)')
    if ($found.Count -eq 0) { return $null }
    $last = $found[$found.Count - 1].Groups[1].Value
    if ($last -eq '-inf') { return $null }
    return [double]::Parse($last, [Globalization.CultureInfo]::InvariantCulture)
}

# ---- Rust ----

function Get-Version([string]$Text) {
    if ($Text -match '(\d+)\.(\d+)(?:\.(\d+))?') {
        $patch = if ($Matches[3]) { [int]$Matches[3] } else { 0 }
        return [version]::new([int]$Matches[1], [int]$Matches[2], $patch)
    }
    return $null
}
function Get-MinRust {
    $m = Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^rust-version\s*=\s*"([^"]+)"' | Select-Object -First 1
    return $m.Matches[0].Groups[1].Value
}
function Get-RustVersion {
    if (-not ((Have 'rustc') -and (Have 'cargo'))) { return $null }
    return Get-Version (Output rustc --version)
}
function Test-Rust {
    $v = Get-RustVersion
    return ($null -ne $v) -and ($v -ge (Get-Version (Get-MinRust)))
}

# ---- What this script added ----

function Read-State {
    $state = @{}
    if (Test-Path $StateFile) {
        try {
            (Get-Content $StateFile -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $state[$_.Name] = $_.Value }
        } catch { }
    }
    return $state
}

# Note something this script added, for uninstall.ps1. Lists (winget
# packages, files) grow; other keys are set.
function Record([string]$Key, $Value) {
    if ($Value -is [array]) {
        $have = @()
        if ($script:State.ContainsKey($Key)) { $have = @($script:State[$Key]) }
        foreach ($v in $Value) { if ($have -notcontains $v) { $have += $v } }
        $script:State[$Key] = $have
    } else {
        $script:State[$Key] = $Value
    }
    New-Item -ItemType Directory -Force (Split-Path $StateFile) | Out-Null
    $script:State | ConvertTo-Json -Depth 4 | Set-Content $StateFile -Encoding UTF8
}

# ---- Steps ----

function Step-Dump {
    Step (T 'look_dump')
    New-Item -ItemType Directory -Force $Game | Out-Null
    $disc = Join-Path $Game 'Freekstyle.bin'
    if (Test-Path $disc) {
        Good (T 'found_dump')
    } else {
        $dumps = @(Get-ChildItem $Game -File | Where-Object { $_.Extension -in '.bin', '.iso' } | Sort-Object Name)
        if ($dumps.Count -eq 0) { throw [Failure]::new((T 'no_dump'), $false) }
        if ($dumps.Count -gt 1) { throw [Failure]::new((T 'many_dumps' (($dumps | ForEach-Object Name) -join ', ')), $false) }
        # A hard link: no copy, no administrator (unlike a symbolic link),
        # and deleting it later leaves the dump.
        try {
            New-Item -ItemType HardLink -Path $disc -Target $dumps[0].FullName | Out-Null
        } catch {
            Copy-Item $dumps[0].FullName $disc
        }
        Record 'files' @('game\Freekstyle.bin')
        Good (T 'using_dump' ('game\' + $dumps[0].Name))
    }
    $free = [math]::Floor((Get-PSDrive (Split-Path -Qualifier $Root).TrimEnd(':')).Free / 1e9)
    if ($free -lt $SpaceGB) {
        Warn (T 'space' $free $SpaceGB)
        if (-not (Ask (T 'continue') $false)) { exit 1 }
    }
}

# (key, label, present, needed to build) for each requirement.
function Get-Requirements {
    return @(
        [pscustomobject]@{ Key = 'compiler'; Label = (T 'req_compiler'); Present = (Test-Msvc); Needed = $true },
        [pscustomobject]@{ Key = 'cmake'; Label = (T 'req_cmake'); Present = (Have 'cmake'); Needed = $true },
        [pscustomobject]@{ Key = 'ffmpeg'; Label = (T 'req_ffmpeg'); Present = (Have 'ffmpeg'); Needed = $false }
    )
}

function Step-Tools {
    Step (T 'check_tools')
    $reqs = Get-Requirements
    foreach ($r in $reqs) { if ($r.Present) { Good $r.Label } else { Bad $r.Label } }
    # Vulkan comes with the graphics driver, which this can't pick for you;
    # it isn't needed to build, only to play.
    if (Test-Vulkan) { Good (T 'req_vulkan') } else { Bad (T 'req_vulkan'); Warn (T 'no_vulkan') }
    $missing = @($reqs | Where-Object { -not $_.Present -and $WingetPackages.ContainsKey($_.Key) })
    if ($missing.Count) {
        if (-not (Have 'winget')) {
            if (@($missing | Where-Object Needed).Count) {
                throw [Failure]::new((T 'no_winget' (($missing | ForEach-Object Key) -join ', ')), $false)
            }
            Warn (T 'no_winget' (($missing | ForEach-Object Key) -join ', '))
        } else {
            $ids = $missing | ForEach-Object { $WingetPackages[$_.Key].Id }
            $question = T 'install_now' ('winget install ' + ($ids -join ' '))
            if ($missing | Where-Object Key -eq 'compiler') { $question += "`n" + (T 'vs_note') }
            if (Ask $question) {
                foreach ($r in $missing) {
                    $p = $WingetPackages[$r.Key]
                    $ok = TryRun (@('winget', 'install', '--exact', '--id', $p.Id, '--source', 'winget',
                                   '--accept-package-agreements', '--accept-source-agreements') + $p.Args)
                    # Only what this installed is uninstall.ps1's to remove;
                    # what winget says was there already is not.
                    if ($ok) { Record 'winget' @($p.Id) }
                }
                Update-Path
                $left = @(Get-Requirements | Where-Object { -not $_.Present -and $_.Needed } | ForEach-Object Label)
                if ($left.Count) { throw [Failure]::new((T 'still_missing' ($left -join ', ')), $false) }
            } elseif (@($missing | Where-Object Needed).Count) {
                throw [Failure]::new((T 'cant_build'), $false)
            }
        }
    }
    if (-not (Have 'ffmpeg') -and (Ask (T 'get_ffmpeg'))) { Get-Ffmpeg }
    Step-Rust
}

# ffmpeg.exe and ffprobe.exe into target\release, next to the game.
function Get-Ffmpeg {
    $zip = Join-Path $Root 'target\ffmpeg.zip'
    $unpacked = Join-Path $Root 'target\ffmpeg'
    try {
        New-Item -ItemType Directory -Force $Release | Out-Null
        Write-Host "  > $FfmpegUrl" -ForegroundColor DarkGray
        Write-Log "> download $FfmpegUrl"
        Invoke-WebRequest $FfmpegUrl -OutFile $zip -UseBasicParsing
        Expand-Archive $zip $unpacked -Force
        foreach ($exe in 'ffmpeg.exe', 'ffprobe.exe') {
            $found = Get-ChildItem $unpacked -Recurse -Filter $exe | Select-Object -First 1
            if (-not $found) { throw "$exe not in the download" }
            Copy-Item $found.FullName $Release -Force
        }
        Update-Path
        Good ('ffmpeg: ' + ((Output ffmpeg -version) -split "`n")[0])
    } catch {
        Write-Log "error: $_"
        Warn (T 'ffmpeg_failed' $_)
    } finally {
        Remove-Item $zip, $unpacked -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Step-Rust {
    $min = Get-MinRust
    if (-not (Test-Rust)) {
        $current = Get-RustVersion
        if ($current) { Say (T 'rust_old' $current $min) } else { Say (T 'rust_none') }
        if (Have 'rustup') {
            if (-not (Ask (T 'rustup_update'))) { throw [Failure]::new((T 'rust_needed' $min), $false) }
            Run @('rustup', 'default', 'stable')
            Run @('rustup', 'update', 'stable')
        } elseif (Ask (T 'rustup_install')) {
            $init = Join-Path $Root 'target\rustup-init.exe'
            New-Item -ItemType Directory -Force (Split-Path $init) | Out-Null
            Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile $init -UseBasicParsing
            Run @($init, '-y', '--profile', 'minimal')
            Record 'rustup' $true
            Update-Path
        } else {
            throw [Failure]::new((T 'rust_needed' $min), $false)
        }
        if (-not (Test-Rust)) {
            $found = (Output rustc --version).Trim()
            if (-not $found) { $found = '-' }
            throw [Failure]::new((T 'rust_still' $min $found), $false)
        }
    }
    Good ('{0}: {1}' -f (T 'rust'), (Output rustc --version).Trim())
}

function Step-Program {
    Step (T 'copy_prog')
    $slus = Join-Path $Game 'SLUS_203.67'
    if (-not (Test-Path $slus)) {
        Run @('cargo', 'run', '--release', '-q', '-p', 'ee-runtime', '--bin', 'disc-extract')
        Record 'files' @('game\SLUS_203.67')
    }
    if ((Get-FileHash $slus -Algorithm SHA256).Hash.ToLower() -ne $SlusSha256) { throw [Failure]::new((T 'bad_slus'), $false) }
    Good 'game\SLUS_203.67 (SLUS-20367)'
}

function Step-Build {
    Step (T 'building')
    Run @('cargo', 'run', '--release', '-q', '-p', 'ee-recomp', '--', 'game/SLUS_203.67', 'freekstyle/src/gen',
         'freekstyle/hle.txt', 'freekstyle/seeds.txt', 'freekstyle/vu_dumps')
    Run @('cargo', 'build', '--release', '-p', 'freekstyle', '-p', 'freekstyle-launcher', '-p', 'flm')
}

function Step-Pictures {
    Step (T 'pictures')
    if (Get-ChildItem (Join-Path $Game 'images\*.png') -ErrorAction SilentlyContinue) {
        Good (T 'already')
    } else {
        Run @('cargo', 'run', '--release', '-q', '-p', 'ee-runtime', '--bin', 'lsc2png')
        Record 'files' @('game\images')
    }
}

function Step-Movies {
    Step (T 'movies')
    $movies = Join-Path $Game 'movies'
    if (-not (Have 'ffmpeg')) {
        Warn (T 'no_ffmpeg')
        return
    }
    if ((Test-Path (Join-Path $movies 'hd\intro.mkv')) -or (Test-Path (Join-Path $movies 'intro.mkv'))) {
        Good (T 'already')
        Repair-MovieHeadroom $movies
        return
    }
    if (-not (Test-FfmpegMpeg2)) {
        Warn (T 'no_mpeg2')
        return
    }
    $flm = Join-Path $movies 'flm'
    $hd = Join-Path $movies 'hd'
    # Only what this step creates is recorded: game\movies may also hold
    # movies made by hand.
    $before = @(@($flm, $hd) | Where-Object { Test-Path $_ })
    $before += @(Get-ChildItem (Join-Path $movies '*.mkv'), (Join-Path $hd '*.mkv') -ErrorAction SilentlyContinue | ForEach-Object FullName)
    try {
        Convert-Movies $movies $flm
    } finally {
        $made = @(@($flm, $hd) + @(Get-ChildItem (Join-Path $movies '*.mkv') -ErrorAction SilentlyContinue | ForEach-Object FullName) |
                  Where-Object { (Test-Path $_) -and ($before -notcontains $_) })
        if ($before -contains $hd) {
            $made += @(Get-ChildItem (Join-Path $hd '*.mkv') -ErrorAction SilentlyContinue | ForEach-Object FullName | Where-Object { $before -notcontains $_ })
        }
        if ($made.Count) { Record 'files' @($made | ForEach-Object { $_.Substring($Root.Length).TrimStart('\') }) }
    }
}

# Movies converted before flm2mkv left headroom go over full scale after
# Opus encoding, and the game's 16-bit decode clips that into a crackle.
# Re-encode just their audio about 3 dB lower; the video is copied untouched.
function Repair-MovieHeadroom([string]$Movies) {
    $fixed = 0
    $all = @(Get-ChildItem (Join-Path $Movies '*.mkv') -ErrorAction SilentlyContinue) + @(Get-ChildItem (Join-Path $Movies 'hd\*.mkv') -ErrorAction SilentlyContinue)
    foreach ($mkv in $all) {
        $peak = Get-MoviePeak $mkv.FullName
        if ($null -eq $peak -or $peak -lt -0.1) { continue }
        $tmp = Join-Path $mkv.DirectoryName ($mkv.BaseName + '.headroom.mkv')
        $gain = '{0:0.00}dB' -f (-3.0 - $peak)
        $gain = $gain.Replace(',', '.')
        $ok = TryRun @('ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', $mkv.FullName, '-map', '0', '-c', 'copy',
                       '-af', "volume=$gain", '-c:a', 'libopus', '-b:a', '160k', $tmp) -Quiet
        if ($ok -and (Test-Path $tmp)) {
            Move-Item $tmp $mkv.FullName -Force
            $fixed++
        } else {
            Remove-Item $tmp -ErrorAction SilentlyContinue
        }
    }
    if ($fixed) { Good (T 'movie_audio' $fixed) }
}

function Convert-Movies([string]$Movies, [string]$Flm) {
    foreach ($m in $MovieList) {
        $folder, $name = $m.Split('/')
        if (-not (Test-Path (Join-Path $Flm "$name.FLM"))) {
            Run @('cargo', 'run', '--release', '-q', '-p', 'ee-runtime', '--bin', 'disc-extract', '--',
                 'game/Freekstyle.bin', "\SXDATA\$folder\$name.FLM", $Flm) -Quiet
        }
    }
    $flm2mkv = Join-Path $Root 'target\release\flm2mkv.exe'
    $hd = Join-Path $Movies 'hd'
    # The CRT look needs ffmpeg's libplacebo filter and a Vulkan GPU (not a
    # software one, as in most virtual machines): try a tiny clip first.
    $crt = TryRun @('ffmpeg', '-hide_banner', '-loglevel', 'quiet', '-f', 'lavfi', '-i', 'color=size=64x64:duration=0.1',
                    '-vf', 'libplacebo', '-f', 'null', '-') -Quiet
    if ($crt -and (TryRun @($flm2mkv, '--crt', $Flm, $hd))) {
        Good (T 'crt')
        return
    }
    Warn (T 'no_crt')
    Remove-Item $hd -Recurse -Force -ErrorAction SilentlyContinue
    if (-not (TryRun @($flm2mkv, $Flm, $Movies))) { Warn (T 'movies_failed') }
}

# A Windows icon made of the PNG pictures (icons hold PNGs since Vista).
function New-Icon([string]$Folder, [string]$Out) {
    $sizes = 16, 24, 32, 48, 64, 128, 256
    $images = foreach ($s in $sizes) {
        $png = Join-Path $Folder "$s.png"
        if (Test-Path $png) { [pscustomobject]@{ Size = $s; Data = [IO.File]::ReadAllBytes($png) } }
    }
    $images = @($images)
    $stream = New-Object IO.MemoryStream
    $w = New-Object IO.BinaryWriter($stream)
    $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($i in $images) {
        $edge = if ($i.Size -ge 256) { 0 } else { $i.Size }   # 0 means 256
        $w.Write([byte]$edge); $w.Write([byte]$edge); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$i.Data.Length); $w.Write([uint32]$offset)
        $offset += $i.Data.Length
    }
    foreach ($i in $images) { $w.Write($i.Data) }
    $w.Flush()
    [IO.File]::WriteAllBytes($Out, $stream.ToArray())
}

# Where a program really is: winget puts links to ffmpeg in a folder on
# PATH, and a copy needs the file they point at.
function Resolve-Program([string]$Name) {
    $cmd = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $cmd) { return $null }
    $item = Get-Item $cmd.Source
    if ($item.LinkType -and $item.Target) {
        $target = @($item.Target)[0]
        if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $item.DirectoryName $target }
        return (Resolve-Path $target).Path
    }
    return $item.FullName
}

function Step-Install {
    Step (T 'installing')
    if ($System) { $where = '1' }
    elseif ($Here) { $where = '2' }
    else {
        Say ((T 'opt_system' $Prefix).Trim())
        Say ((T 'opt_here').Trim())
        $where = Choose (T 'choose') '1'
    }
    if ($where -ne '1') { return $false }
    # Gathered here, as yourself: what the administrator half copies.
    $icon = Join-Path $Root 'target\freekstyle.ico'
    New-Icon (Join-Path $Root 'freekstyle\assets\launcher') $icon
    $plan = @{ source = $Root; icon = $icon; ffmpeg = (Resolve-Program 'ffmpeg'); ffprobe = (Resolve-Program 'ffprobe') }
    $planFile = Join-Path $Root 'target\install-plan.json'
    $plan | ConvertTo-Json | Set-Content $planFile -Encoding UTF8
    Say (T 'copying_disc')
    if (Test-Admin) {
        $ok = (Install-System $planFile) -eq 0
    } else {
        $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-ElevatedInstall', "`"$planFile`"")
        try {
            $p = Start-Process powershell.exe -Verb RunAs -ArgumentList $argv -Wait -PassThru -WindowStyle Hidden
            $ok = $p.ExitCode -eq 0
        } catch {
            $ok = $false   # the UAC prompt was declined
        }
    }
    if (-not $ok) {
        Warn (T 'elevate_failed')
        return $false
    }
    # Settings made here carry over (Program Files isn't writable, so the
    # game keeps them in %APPDATA%\freekstyle).
    $config = Join-Path $env:APPDATA 'freekstyle'
    $toml = Join-Path $Root 'freekstyle.toml'
    if ((Test-Path $toml) -and -not (Test-Path (Join-Path $config 'freekstyle.toml'))) {
        New-Item -ItemType Directory -Force $config | Out-Null
        Copy-Item $toml $config
    }
    Record 'system' $Prefix
    return $true
}

# The administrator half of the install: copy the game into Program Files,
# add the Start menu shortcut and the Installed apps entry. Returns an exit
# code; problems go to install.log.
function Install-System([string]$PlanFile) {
    try {
        $plan = Get-Content $PlanFile -Raw | ConvertFrom-Json
        $src = $plan.source
        $game = Join-Path $src 'game'
        $release = Join-Path $src 'target\release'
        Write-Log "`r`n== install to $Prefix (administrator)"
        if (Test-Path (Join-Path $Prefix 'game')) { Remove-Item (Join-Path $Prefix 'game') -Recurse -Force }
        New-Item -ItemType Directory -Force (Join-Path $Prefix 'game\movies') | Out-Null
        Copy-Item (Join-Path $release 'freekstyle.exe'), (Join-Path $release 'freekstyle-launcher.exe') $Prefix -Force
        Copy-Item (Join-Path $game 'SLUS_203.67'), (Join-Path $game 'Freekstyle.bin') (Join-Path $Prefix 'game')
        if (Test-Path (Join-Path $game 'images')) { Copy-Item (Join-Path $game 'images') (Join-Path $Prefix 'game') -Recurse }
        if (Test-Path (Join-Path $game 'movies\hd')) { Copy-Item (Join-Path $game 'movies\hd') (Join-Path $Prefix 'game\movies') -Recurse }
        if (Test-Path (Join-Path $game 'textures\hd')) {
            New-Item -ItemType Directory -Force (Join-Path $Prefix 'game\textures') | Out-Null
            Copy-Item (Join-Path $game 'textures\hd') (Join-Path $Prefix 'game\textures') -Recurse
        }
        Get-ChildItem (Join-Path $game 'movies\*.mkv') -ErrorAction SilentlyContinue | Copy-Item -Destination (Join-Path $Prefix 'game\movies')
        # The game plays its movies through ffmpeg; Windows finds a program
        # next to the game first, so a copy here works without PATH.
        foreach ($tool in @($plan.ffmpeg, $plan.ffprobe)) {
            if ($tool -and (Test-Path $tool)) { Copy-Item $tool $Prefix -Force }
        }
        Copy-Item $plan.icon (Join-Path $Prefix 'freekstyle.ico') -Force
        # The uninstaller goes along, for when the source folder is gone.
        Copy-Item (Join-Path $src 'windows\uninstall.ps1') $Prefix -Force

        $launcher = Join-Path $Prefix 'freekstyle-launcher.exe'
        $shell = New-Object -ComObject WScript.Shell
        $lnk = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'Freekstyle.lnk'))
        $lnk.TargetPath = $launcher
        $lnk.WorkingDirectory = $Prefix
        $lnk.IconLocation = (Join-Path $Prefix 'freekstyle.ico') + ',0'
        $lnk.Description = T 'shortcut'
        $lnk.Save()

        $version = (Select-String -Path (Join-Path $src 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
        $kb = [int]((Get-ChildItem $Prefix -Recurse -File | Measure-Object Length -Sum).Sum / 1KB)
        $uninstall = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "{0}"' -f (Join-Path $Prefix 'uninstall.ps1')
        New-Item -Path $UninstallKey -Force | Out-Null
        $values = [ordered]@{
            DisplayName = 'Freekstyle'; DisplayVersion = $version; Publisher = 'Freekstyle native port'
            DisplayIcon = (Join-Path $Prefix 'freekstyle.ico'); InstallLocation = $Prefix
            UninstallString = $uninstall; QuietUninstallString = "$uninstall -Yes"
            URLInfoAbout = 'https://github.com/Broly1/freekstyle-native-port'
        }
        foreach ($k in $values.Keys) { New-ItemProperty -Path $UninstallKey -Name $k -Value $values[$k] -PropertyType String -Force | Out-Null }
        foreach ($k in 'NoModify', 'NoRepair') { New-ItemProperty -Path $UninstallKey -Name $k -Value 1 -PropertyType DWord -Force | Out-Null }
        New-ItemProperty -Path $UninstallKey -Name EstimatedSize -Value $kb -PropertyType DWord -Force | Out-Null
        Write-Log "installed to $Prefix"
        return 0
    } catch {
        Write-Log "error: $_"
        return 1
    }
}

function Main {
    if ($ElevatedInstall) { exit (Install-System $ElevatedInstall) }
    if ($Uninstall) {
        & (Join-Path $PSScriptRoot 'uninstall.ps1') -Yes:$Yes
        exit $LASTEXITCODE
    }
    Write-Log ("`r`n==== {0}: windows\install.ps1 {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), ($PSBoundParameters.Keys -join ' '))
    if (Test-Admin) {
        Warn (T 'admin')
        if (-not (Ask (T 'continue') $false)) { exit 1 }
    }
    $script:State = Read-State
    $script:State['source'] = $Root
    Update-Path
    $start = Get-Date
    try {
        Step-Dump
        Step-Tools
        Step-Program
        Step-Build
        Step-Pictures
        Step-Movies
        $installed = Step-Install
    } catch [Failure] {
        Write-Host ''
        Write-Host (T 'error') -ForegroundColor Red -NoNewline
        Write-Host " $($_.Exception.Message)"
        Write-Log "error: $($_.Exception.Message)"
        if ($_.Exception.Logged) { Write-Host ('  ' + (T 'see_log' $Log)) }
        exit 1
    } catch {
        Write-Host ''
        Write-Host (T 'error') -ForegroundColor Red -NoNewline
        Write-Host " $_"
        Write-Log "error: $_"
        Write-Host ('  ' + (T 'see_log' $Log))
        exit 1
    }
    $took = (Get-Date) - $start
    Write-Host ''
    Write-Host (T 'done' ('{0}m{1:00}s' -f [int][math]::Floor($took.TotalMinutes), $took.Seconds)) -ForegroundColor Green
    if ($installed) { Write-Host (T 'done_system') } else { Write-Host (T 'done_here') }
    Write-Host ''
    exit 0
}

Main

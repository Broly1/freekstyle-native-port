#!/usr/bin/env python3
"""Remove Freekstyle, without touching anything else on the system.

It lists what it finds and asks about each part:
  - the system install (/opt/freekstyle, its menu entries, icons and the
    freekstyle-launcher command)                                 [default: yes]
  - menu entries the launcher made for your user                 [default: yes]
  - build output in the source folder (target/, the generated game code,
    and the files linux-install.py recorded creating; never your disc dump)
                                                                 [default: yes]
  - the HD textures made while playing (they are made again as you play)
                                                                 [default: yes]
  - your saves and settings                                      [default: no]

Packages, Rust and RPM Fusion that linux-install.py added to build the game
are left installed, as other software (or a later rebuild) may use them: at
the end it lists those still there, with the command that removes each.

What linux-install.py did is read from ~/.local/state/freekstyle/install.json.
Only files listed there or known to belong to the game are removed.
--dry-run shows what would happen and changes nothing.

It is copied into /opt/freekstyle by a system install, so it also works
after the source folder is gone. Only the Python standard library is used.
"""

import argparse
import glob
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
HOME = Path.home()
STATE = Path(os.environ.get("XDG_STATE_HOME") or HOME / ".local" / "state") / "freekstyle" / "install.json"
DATA = Path(os.environ.get("XDG_DATA_HOME") or HOME / ".local" / "share")
CONFIG = Path(os.environ.get("XDG_CONFIG_HOME") or HOME / ".config")
PREFIX = Path("/opt/freekstyle")
COMMAND = Path("/usr/local/bin/freekstyle-launcher")
MENU_NAMES = ("freekstyle.desktop", "freekstyle-launcher.desktop")

# ---- Messages ----

def _language():
    """As in linux-install.py and the launcher: 0 English, 1 Portuguese (Brazil),
    2 Spanish, 3 French."""
    for v in ("LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(v)
        if value:
            return {"pt": 1, "es": 2, "fr": 3}.get(value[:2], 0)
    return 0

LANG = _language()

MESSAGES = {
    "yes_no": ("[Y/n]", "[S/n]", "[S/n]", "[O/n]"),
    "yes_no_no": ("[y/N]", "[s/N]", "[s/N]", "[o/N]"),
    "error": ("error:", "erro:", "error:", "erreur :"),
    "root": ("run this as your own user, not as root: it asks for sudo when it needs it.",
             "rode como seu usuário, não como root: ele pede sudo quando precisa.",
             "ejecútalo como tu usuario, no como root: pide sudo cuando lo necesita.",
             "lancez-le avec votre utilisateur, pas en root : il demande sudo quand il en a besoin."),
    "title": ("Uninstall Freekstyle", "Desinstalar o Freekstyle", "Desinstalar Freekstyle", "Désinstaller Freekstyle"),
    "nothing": ("Nothing of Freekstyle was found to remove.", "Nada do Freekstyle foi encontrado para remover.",
                "No se encontró nada de Freekstyle para quitar.", "Rien de Freekstyle n'a été trouvé à supprimer."),
    "no_state": ("(no record from linux-install.py was found, so only the game's own files are offered)",
                 "(nenhum registro do linux-install.py foi encontrado, então só os arquivos do próprio jogo são oferecidos)",
                 "(no se encontró registro de linux-install.py, así que solo se ofrecen los archivos del propio juego)",
                 "(aucun relevé de linux-install.py trouvé, seuls les fichiers du jeu lui-même sont proposés)"),
    "system": ("The installed game: %s, the app menu entry and the freekstyle-launcher command (needs sudo)",
               "O jogo instalado: %s, o item do menu e o comando freekstyle-launcher (precisa de sudo)",
               "El juego instalado: %s, la entrada del menú y el comando freekstyle-launcher (necesita sudo)",
               "Le jeu installé : %s, l'entrée du menu et la commande freekstyle-launcher (demande sudo)"),
    "user_menu": ("Menu entries the launcher made for your user", "Itens de menu que o launcher criou para o seu usuário",
                  "Entradas de menú que el launcher creó para tu usuario", "Entrées de menu créées par le lanceur pour votre utilisateur"),
    "build": ("Build output in %s (the compiled game, generated code, extracted and converted files; your disc dump stays)",
              "Arquivos de compilação em %s (o jogo compilado, código gerado, arquivos extraídos e convertidos; sua imagem do disco fica)",
              "Archivos de compilación en %s (el juego compilado, código generado, archivos extraídos y convertidos; tu imagen del disco se queda)",
              "Fichiers de compilation dans %s (le jeu compilé, le code généré, les fichiers extraits et convertis ; votre image disque reste)"),
    "hd": ("HD textures made while playing: %s (made again as you play)", "Texturas em HD feitas enquanto você jogava: %s (feitas de novo enquanto joga)",
           "Texturas en HD hechas mientras jugabas: %s (se hacen de nuevo mientras juegas)", "Textures HD faites pendant vos parties : %s (refaites en jouant)"),
    "data": ("Your saves and settings: %s", "Seus saves e configurações: %s", "Tus partidas guardadas y ajustes: %s", "Vos sauvegardes et réglages : %s"),
    "left": ("Left installed (linux-install.py added them to build the game; other software may use them too).\nTo remove them yourself:",
             "Continuam instalados (o linux-install.py os adicionou para compilar o jogo; outros programas podem usá-los).\nPara removê-los você mesmo:",
             "Siguen instalados (linux-install.py los añadió para compilar el juego; otros programas pueden usarlos).\nPara quitarlos tú mismo:",
             "Restent installés (linux-install.py les a ajoutés pour compiler le jeu ; d'autres logiciels peuvent s'en servir).\nPour les supprimer vous-même :"),
    "left_pkgs": ("packages", "pacotes", "paquetes", "paquets"),
    "left_rpmfusion": ("RPM Fusion (for the full ffmpeg)", "RPM Fusion (para o ffmpeg completo)",
                       "RPM Fusion (para el ffmpeg completo)", "RPM Fusion (pour le ffmpeg complet)"),
    "remove_q": ("Remove?", "Remover?", "¿Quitar?", "Supprimer ?"),
    "removing": ("Removing", "Removendo", "Quitando", "Suppression"),
    "dry": ("dry run: nothing was changed.", "simulação: nada foi alterado.", "simulación: no se cambió nada.", "simulation : rien n'a été modifié."),
    "done": ("Done. Freekstyle has been removed.", "Pronto. O Freekstyle foi removido.", "Listo. Freekstyle se ha quitado.", "Terminé. Freekstyle a été supprimé."),
    "done_partial": ("Done. Kept: %s", "Pronto. Mantido: %s", "Listo. Se conserva: %s", "Terminé. Conservé : %s"),
    "failed": ("this command failed (exit %s): %s", "este comando falhou (saída %s): %s", "este comando falló (salida %s): %s",
               "cette commande a échoué (code %s) : %s"),
}

def t(key, *args):
    text = MESSAGES[key][LANG]
    return text % args if args else text

COLOUR = sys.stdout.isatty() and "NO_COLOR" not in os.environ

def paint(code, text):
    return "\033[%sm%s\033[0m" % (code, text) if COLOUR else text

# ---- Helpers ----

def have(name):
    return shutil.which(name) is not None

def quiet(cmd, env=None):
    """Run a command silently; True if it succeeded."""
    try:
        return subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
                              env=env).returncode == 0
    except OSError:
        return False

def output(cmd, env=None, stderr=False):
    """A command's output (with stderr: both streams), or "" if it can't run."""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, errors="replace", stdin=subprocess.DEVNULL, env=env)
        return r.stdout + r.stderr if stderr else r.stdout
    except OSError:
        return ""

def size(paths):
    total = 0
    for p in paths:
        p = Path(p)
        if p.is_symlink() or p.is_file():
            total += p.lstat().st_size
        elif p.is_dir():
            for root, _, files in os.walk(p):
                for f in files:
                    try:
                        total += os.lstat(os.path.join(root, f)).st_size
                    except OSError:
                        pass
    return total

def human(n):
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1000 or unit == "GB":
            return ("%d %s" % (n, unit)) if unit == "B" else ("%.1f %s" % (n, unit))
        n /= 1000.0

def load_state():
    try:
        return json.loads(STATE.read_text())
    except (OSError, ValueError):
        return {}

def is_source(folder):
    """A Freekstyle source folder: the paths below are only deleted there."""
    try:
        return (folder / "linux-install.py").is_file() and 'name = "freekstyle"' in (folder / "freekstyle" / "Cargo.toml").read_text()
    except OSError:
        return False

def located_in(path, folder):
    """`path` itself (a link counts where it is, not where it points) lies
    inside `folder`, and is not the folder: guards every deletion in the
    source folder."""
    where = Path(os.path.realpath(Path(path).parent)) / Path(path).name
    return Path(os.path.realpath(folder)) in where.parents

# ---- Left installed ----

def pkg_installed(family, pkg):
    if family == "arch":
        return quiet(["pacman", "-Q", pkg])
    if family == "debian":
        return "install ok installed" in output(["dpkg-query", "-W", "-f=${Status}", pkg])
    return quiet(["rpm", "-q", pkg])

def is_graphics(pkg):
    """A Vulkan/GPU driver or the Vulkan loader: games and desktops use
    them, so they are never suggested for removal."""
    return pkg.startswith(("vulkan-", "libvulkan", "mesa-vulkan", "nvidia"))

def remove_hint(family, pkgs):
    """Marks them as no longer wanted, then removes those nothing else
    needs: a plain remove would also take whatever depends on them."""
    names = " ".join(pkgs)
    if family == "arch":
        return "sudo pacman -D --asdeps %s && pacman -Qdtq | sudo pacman -Rns -" % names
    if family == "debian":
        return "sudo apt-mark auto %s && sudo apt autoremove" % names
    if family == "fedora":
        return "sudo dnf mark dependency %s && sudo dnf autoremove" % names
    return "sudo zypper remove --clean-deps %s" % names

def leftovers(state):
    """(what, command) for each thing linux-install.py added that is still
    installed."""
    family = state.get("family")
    out = []
    pkgs = [p for p in state.get("packages", []) if family and not is_graphics(p) and pkg_installed(family, p)]
    if pkgs:
        out.append((t("left_pkgs") + ": " + " ".join(pkgs), remove_hint(family, pkgs)))
    if state.get("rustup") and (HOME / ".cargo" / "bin" / "rustup").exists():
        out.append(("Rust (rustup, ~/.cargo ~/.rustup)", "rustup self uninstall"))
    # RPM Fusion only while nothing else from it is installed (removing it
    # would stop their updates); the full ffmpeg goes back to Fedora's
    # ffmpeg-free first, if it is still the one installed.
    if state.get("rpmfusion") and quiet(["rpm", "-q", "rpmfusion-free-release"]):
        others = [line.split()[0] for line in output(["rpm", "-qa", "--qf", "%{NAME} %{VENDOR}\\n"]).splitlines()
                  if "RPM Fusion" in line and not line.startswith(("rpmfusion-", "ffmpeg"))]
        if not others:
            swap = quiet(["rpm", "-q", "ffmpeg"]) and not quiet(["rpm", "-q", "ffmpeg-free"])
            out.append((t("left_rpmfusion"), ("sudo dnf swap ffmpeg ffmpeg-free --allowerasing && " if swap else "")
                        + "sudo dnf remove rpmfusion-free-release"))
    return out

# ---- The parts ----

class Part:
    def __init__(self, key, label, default, size=0, note=None):
        self.key, self.label, self.default, self.size, self.note = key, label, default, size, note

def system_paths():
    """The system install's files, each checked to be the game's."""
    paths = []
    if (PREFIX / "freekstyle-launcher").exists():
        paths.append(PREFIX)
    if COMMAND.is_symlink() and os.readlink(COMMAND).startswith(str(PREFIX)):
        paths.append(COMMAND)
    for name in MENU_NAMES:
        entry = Path("/usr/share/applications") / name
        if entry.is_file() and str(PREFIX) in entry.read_text(errors="replace"):
            paths.append(entry)
    if paths:
        paths += glob.glob("/usr/share/icons/hicolor/*/apps/freekstyle-*.png")
    return paths

def user_menu_paths():
    paths = [DATA / "applications" / n for n in MENU_NAMES if (DATA / "applications" / n).is_file()]
    return paths + [Path(p) for p in glob.glob(str(DATA / "icons" / "hicolor" / "*" / "apps" / "freekstyle-*.png"))]

def build_paths(state):
    """Build output in the source folder: target/ and the generated code
    (pure build output), plus the files linux-install.py recorded creating. The
    disc dump is never among them."""
    source = Path(state.get("source") or HERE)
    if not is_source(source):
        return source, []
    paths = [source / "target", source / "freekstyle" / "src" / "gen", source / "install.log"]
    paths += [source / f for f in state.get("files", [])]
    # The disc dump, wherever game/Freekstyle.bin leads: never deleted, nor
    # any folder holding it (the link itself may go: unlinking a link
    # leaves its target).
    dump = Path(os.path.realpath(source / "game" / "Freekstyle.bin"))
    out = []
    for p in paths:
        if not (p.exists() or p.is_symlink()) or not located_in(p, source):
            continue
        if not p.is_symlink() and (Path(os.path.realpath(p)) == dump or Path(os.path.realpath(p)) in dump.parents):
            continue
        out.append(p)
    return source, out

def hd_paths():
    """The texture pack the game builds while playing, and what goes with
    it (freek_config::data_dir in the game)."""
    base = DATA / "freekstyle"
    paths = [base / "textures", base / "hd.log", base / "hd-building.lock"]
    return [p for p in paths if p.exists()]

def data_paths(state):
    paths = [DATA / "freekstyle", CONFIG / "freekstyle"]
    source = Path(state.get("source") or HERE)
    if is_source(source):
        paths.append(source / "freekstyle.toml")
    return [p for p in paths if p.exists()]

class Uninstaller:
    def __init__(self, args):
        self.args = args
        self.state = load_state()

    def run(self, cmd, sudo=False):
        cmd = [str(c) for c in cmd]
        if sudo:
            cmd = ["sudo"] + cmd
        print(paint("2", "  $ " + " ".join(cmd if len(cmd) < 12 else cmd[:11] + ["..."])))
        if self.args.dry_run:
            return True
        code = subprocess.call(cmd)
        if code != 0:
            print("  " + paint("31", t("error")) + " " + t("failed", code, " ".join(cmd[:11])))
        return code == 0

    def ask(self, question, default):
        default = default or self.args.all
        hint = t("yes_no") if default else t("yes_no_no")
        if self.args.yes or self.args.dry_run:
            print("  %s %s %s" % (question, hint, "y" if default else "n"))
            return default
        try:
            reply = input("  %s %s " % (question, hint)).strip().lower()
        except EOFError:
            print()
            return False
        return default if not reply else reply[0] in "yso"  # yes, sim/sí, oui

    def parts(self):
        parts = []
        system = system_paths()
        if system:
            parts.append(Part("system", t("system", PREFIX), True, size(system)))
        menu = user_menu_paths()
        if menu:
            parts.append(Part("user_menu", t("user_menu"), True, size(menu)))
        source, build = build_paths(self.state)
        if build:
            parts.append(Part("build", t("build", source), True, size(build)))
        hd = hd_paths()
        if hd:
            parts.append(Part("hd", t("hd", (DATA / "freekstyle" / "textures").as_posix().replace(str(HOME), "~")), True, size(hd)))
        data = data_paths(self.state)
        if data:
            parts.append(Part("data", t("data", ", ".join(str(p).replace(str(HOME), "~") for p in data)), False, size(data)))
        return parts

    def main(self):
        print(paint("1", t("title")))
        if not self.state:
            print(paint("2", t("no_state")))
        parts = self.parts()
        if not parts:
            print(t("nothing"))
            self.show_leftovers()
            return 0
        chosen, kept = [], []
        for part in parts:
            print()
            print("- " + part.label + (paint("2", "  (%s)" % human(part.size)) if part.size else ""))
            if part.note:
                print("  " + paint("2", part.note))
            (chosen if self.ask(t("remove_q"), part.default) else kept).append(part)
        if chosen:
            print("\n" + paint("1", t("removing")))
        for part in chosen:
            getattr(self, "remove_" + part.key)()
        self.save_state(chosen)
        print()
        if self.args.dry_run:
            print(t("dry"))
        elif kept:
            print(t("done_partial", "; ".join(p.label.split(":")[0].split(" (")[0] for p in kept)))
        else:
            print(paint("1;32", t("done")))
        self.show_leftovers()
        return 0

    def show_leftovers(self):
        left = leftovers(self.state)
        if not left:
            return
        print()
        print(t("left"))
        for what, command in left:
            print("  - " + what)
            print("      " + paint("1", command))

    def remove_system(self):
        paths = system_paths()
        self.run(["rm", "-rf"] + [p for p in paths if "/icons/" not in str(p)], sudo=True)
        icons = [p for p in paths if "/icons/" in str(p)]
        if icons:
            self.run(["rm", "-f"] + icons, sudo=True)
        # Let the desktops see the menu change now, not at next login.
        if have("update-desktop-database"):
            self.run(["update-desktop-database", "-q", "/usr/share/applications"], sudo=True)
        if have("gtk-update-icon-cache"):
            self.run(["gtk-update-icon-cache", "-q", "-t", "-f", "/usr/share/icons/hicolor"], sudo=True)

    def remove_user_menu(self):
        self.delete(user_menu_paths())
        if have("kbuildsycoca6") and not self.args.dry_run:
            quiet(["kbuildsycoca6"])

    def remove_build(self):
        source, paths = build_paths(self.state)
        self.delete(paths)
        # Folders the converted files were in, if nothing else is left there.
        for folder in (source / "game" / "movies" / "hd", source / "game" / "movies"):
            if not self.args.dry_run and folder.is_dir() and not any(folder.iterdir()):
                folder.rmdir()

    def remove_hd(self):
        self.delete(hd_paths())

    def remove_data(self):
        self.delete(data_paths(self.state))

    def delete(self, paths):
        for n, p in enumerate(paths):
            if n < 4 or len(paths) <= 5:
                print(paint("2", "  rm -r " + str(p).replace(str(HOME), "~")))
            elif n == 4:
                print(paint("2", "  ... (+%d)" % (len(paths) - 4)))
            # A path inside one deleted before it is already gone
            # (game/images/hd is recorded after game/images).
            if self.args.dry_run or not (p.exists() or p.is_symlink()):
                continue
            if p.is_dir() and not p.is_symlink():
                shutil.rmtree(p)
            else:
                p.unlink()

    def save_state(self, chosen):
        """Forget what was removed, so a later run doesn't offer it again.
        Once the game and its build output are gone the record goes too
        (the list of what was left installed has just been shown)."""
        if self.args.dry_run or not self.state:
            return
        keys = {p.key for p in chosen}
        if "build" in keys:
            self.state["files"] = []
        # Only once it is really gone (a wrong sudo password leaves it).
        if "system" in keys and not system_paths():
            self.state.pop("system", None)
        if not (self.state.get("system") or self.state.get("files")):
            STATE.unlink(missing_ok=True)
            try:
                STATE.parent.rmdir()
            except OSError:
                pass
        else:
            STATE.write_text(json.dumps(self.state, indent=2))

def main():
    parser = argparse.ArgumentParser(description="Remove Freekstyle; packages and Rust that linux-install.py added stay (see the top of this file).")
    parser.add_argument("-y", "--yes", action="store_true", help="don't ask: take each part's default (with --all: everything)")
    parser.add_argument("--all", action="store_true", help="default to removing everything, your saves and settings included")
    parser.add_argument("--dry-run", action="store_true", help="show what would be removed and change nothing")
    args = parser.parse_args()
    if os.geteuid() == 0:
        print(paint("31", t("error")), t("root"), file=sys.stderr)
        return 1
    try:
        return Uninstaller(args).main()
    except KeyboardInterrupt:
        print()
        return 130

if __name__ == "__main__":
    sys.exit(main())

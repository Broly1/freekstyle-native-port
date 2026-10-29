#!/usr/bin/env python3
"""Remove Freekstyle and what install.py added, without touching anything
else on the system.

It lists what it finds and asks about each part:
  - the system install (/opt/freekstyle, its menu entries, icons and the
    freekstyle-launcher command)                                 [default: yes]
  - menu entries the launcher made for your user                 [default: yes]
  - build output in the source folder (target/, the generated game code,
    and the files install.py recorded creating; never your disc dump)
                                                                 [default: yes]
  - your saves and settings                                      [default: no]
  - packages install.py installed (only those it recorded, and only those
    no other installed software still needs)                     [default: no]
  - Rust, if install.py installed it with rustup                 [default: no]
  - RPM Fusion, if install.py enabled it (Fedora)                [default: no]

What install.py did is read from ~/.local/state/freekstyle/install.json.
Only files and packages listed there or known to belong to the game are
removed. --dry-run shows what would happen and changes nothing.

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
    """As in install.py and the launcher: 0 English, 1 Portuguese (Brazil),
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
    "no_state": ("(no record from install.py was found, so only the game's own files are offered)",
                 "(nenhum registro do install.py foi encontrado, então só os arquivos do próprio jogo são oferecidos)",
                 "(no se encontró registro de install.py, así que solo se ofrecen los archivos del propio juego)",
                 "(aucun relevé d'install.py trouvé, seuls les fichiers du jeu lui-même sont proposés)"),
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
    "data": ("Your saves and settings: %s", "Seus saves e configurações: %s", "Tus partidas guardadas y ajustes: %s", "Vos sauvegardes et réglages : %s"),
    "packages": ("Packages install.py installed: %s", "Pacotes que o install.py instalou: %s",
                 "Paquetes que instaló install.py: %s", "Paquets installés par install.py : %s"),
    "packages_note": ("only those no other installed software needs are removed; the rest stay",
                      "só são removidos os que nenhum outro programa instalado usa; o resto fica",
                      "solo se quitan los que ningún otro programa instalado necesita; el resto se queda",
                      "seuls ceux dont aucun autre logiciel installé n'a besoin sont supprimés ; les autres restent"),
    "rust": ("Rust, installed by install.py with rustup: removes ~/.cargo and ~/.rustup",
             "A Rust, instalada pelo install.py com rustup: remove ~/.cargo e ~/.rustup",
             "Rust, instalado por install.py con rustup: quita ~/.cargo y ~/.rustup",
             "Rust, installé par install.py avec rustup : supprime ~/.cargo et ~/.rustup"),
    "rust_tools": ("also removes these programs installed with cargo: %s", "também remove estes programas instalados com cargo: %s",
                   "también quita estos programas instalados con cargo: %s", "supprime aussi ces programmes installés avec cargo : %s"),
    "rpmfusion": ("RPM Fusion, enabled by install.py for the full ffmpeg (ffmpeg-free comes back)",
                  "O RPM Fusion, ativado pelo install.py para o ffmpeg completo (o ffmpeg-free volta)",
                  "RPM Fusion, activado por install.py para el ffmpeg completo (vuelve ffmpeg-free)",
                  "RPM Fusion, activé par install.py pour le ffmpeg complet (ffmpeg-free revient)"),
    "remove_q": ("Remove?", "Remover?", "¿Quitar?", "Supprimer ?"),
    "removing": ("Removing", "Removendo", "Quitando", "Suppression"),
    "kept_used": ("kept, other software needs it: %s", "mantido, outro programa usa: %s", "se queda, otro programa lo necesita: %s",
                  "conservé, un autre logiciel en a besoin : %s"),
    "not_installed": ("already gone: %s", "já não está instalado: %s", "ya no está instalado: %s", "déjà absent : %s"),
    "kept_drivers": ("kept, graphics drivers other games and apps use too: %s", "mantido, drivers de vídeo que outros jogos e apps também usam: %s",
                     "se queda, drivers gráficos que usan también otros juegos y apps: %s",
                     "conservé, pilotes graphiques utilisés aussi par d'autres jeux et applis : %s"),
    "rpmfusion_kept": ("RPM Fusion kept: other packages from it are installed (%s)", "RPM Fusion mantido: há outros pacotes dele instalados (%s)",
                       "RPM Fusion se queda: hay otros paquetes suyos instalados (%s)", "RPM Fusion conservé : d'autres paquets en viennent (%s)"),
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

def quiet(cmd):
    """Run a command silently; True if it succeeded."""
    try:
        return subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL).returncode == 0
    except OSError:
        return False

def output(cmd):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, errors="replace", stdin=subprocess.DEVNULL).stdout
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
        return (folder / "install.py").is_file() and 'name = "freekstyle"' in (folder / "freekstyle" / "Cargo.toml").read_text()
    except OSError:
        return False

def located_in(path, folder):
    """`path` itself (a link counts where it is, not where it points) lies
    inside `folder`, and is not the folder: guards every deletion in the
    source folder."""
    where = Path(os.path.realpath(Path(path).parent)) / Path(path).name
    return Path(os.path.realpath(folder)) in where.parents

# ---- Packages ----

def pkg_installed(family, pkg):
    if family == "arch":
        return quiet(["pacman", "-Q", pkg])
    if family == "debian":
        return "install ok installed" in output(["dpkg-query", "-W", "-f=${Status}", pkg])
    return quiet(["rpm", "-q", pkg])

def removable(family, pkgs):
    """The package manager's own dry run: removing `pkgs` breaks nothing
    else installed."""
    if family == "arch":
        return quiet(["pacman", "-Rs", "--print", "--noconfirm"] + pkgs)
    if family == "debian":
        return quiet(["dpkg", "--remove", "--dry-run"] + pkgs)
    return quiet(["rpm", "-e", "--test"] + pkgs)

def safe_set(family, pkgs):
    """The largest set found that can go together: a package is added only
    if the dry run still passes with it. Two passes, for packages among
    them that need each other."""
    safe = []
    for _ in range(2):
        for p in pkgs:
            if p not in safe and removable(family, safe + [p]):
                safe.append(p)
    return safe

def is_driver(pkg):
    """A Vulkan/GPU driver. Nothing depends on a driver, so the dry run
    always passes, yet games and desktops use it: never removed."""
    return pkg.startswith(("vulkan-", "libvulkan_", "mesa-vulkan", "nvidia")) and pkg != "vulkan-icd-loader"

def remove_command(family, pkgs):
    """Removal that also takes the dependencies these packages pulled in
    and nothing else needs (pacman -s, zypper --clean-deps; dnf does it by
    default; apt is handled by autoremove_new)."""
    if family == "arch":
        return ["pacman", "-Rs", "--noconfirm"] + pkgs
    if family == "debian":
        return ["apt-get", "remove", "-y"] + pkgs
    if family == "fedora":
        return ["dnf", "remove", "-y"] + pkgs
    return ["zypper", "--non-interactive", "remove", "--clean-deps"] + pkgs

def autoremovable():
    """Packages apt would autoremove now (installed as dependencies of
    something since removed)."""
    out = output(["apt-get", "-s", "autoremove"])
    return {line.split()[1] for line in out.splitlines() if line.startswith("Remv ")}

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
    return paths + glob.glob(str(DATA / "icons" / "hicolor" / "*" / "apps" / "freekstyle-*.png"))

def build_paths(state):
    """Build output in the source folder: target/ and the generated code
    (pure build output), plus the files install.py recorded creating. The
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

def data_paths(state):
    paths = [DATA / "freekstyle", CONFIG / "freekstyle"]
    source = Path(state.get("source") or HERE)
    if is_source(source):
        paths.append(source / "freekstyle.toml")
    return [p for p in paths if p.exists()]

def cargo_tools():
    """Programs in ~/.cargo/bin other than rustup's own."""
    rustup_own = {"cargo", "cargo-clippy", "cargo-fmt", "cargo-miri", "clippy-driver", "rls", "rust-analyzer",
                  "rust-gdb", "rust-gdbgui", "rust-lldb", "rustc", "rustdoc", "rustfmt", "rustup"}
    folder = HOME / ".cargo" / "bin"
    return sorted(p.name for p in folder.iterdir() if p.name not in rustup_own) if folder.is_dir() else []

class Uninstaller:
    def __init__(self, args):
        self.args = args
        self.state = load_state()
        self.family = self.state.get("family")

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
        data = data_paths(self.state)
        if data:
            parts.append(Part("data", t("data", ", ".join(str(p).replace(str(HOME), "~") for p in data)), False, size(data)))
        pkgs = [p for p in self.state.get("packages", []) if self.family and not is_driver(p) and pkg_installed(self.family, p)]
        if pkgs:
            parts.append(Part("packages", t("packages", " ".join(pkgs)), False, note=t("packages_note")))
        if self.state.get("rustup") and (HOME / ".cargo" / "bin" / "rustup").exists():
            tools = cargo_tools()
            parts.append(Part("rust", t("rust"), False, size([HOME / ".cargo", HOME / ".rustup"]),
                              note=t("rust_tools", ", ".join(tools)) if tools else None))
        if self.state.get("rpmfusion") and quiet(["rpm", "-q", "rpmfusion-free-release"]):
            parts.append(Part("rpmfusion", t("rpmfusion"), False))
        return parts

    def main(self):
        print(paint("1", t("title")))
        if not self.state:
            print(paint("2", t("no_state")))
        parts = self.parts()
        if not parts:
            print(t("nothing"))
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
        return 0

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

    def remove_data(self):
        self.delete(data_paths(self.state))

    def delete(self, paths):
        for n, p in enumerate(paths):
            if n < 4 or len(paths) <= 5:
                print(paint("2", "  rm -r " + str(p).replace(str(HOME), "~")))
            elif n == 4:
                print(paint("2", "  ... (+%d)" % (len(paths) - 4)))
            if self.args.dry_run:
                continue
            if p.is_dir() and not p.is_symlink():
                shutil.rmtree(p)
            else:
                p.unlink()

    def remove_packages(self):
        drivers = [p for p in self.state.get("packages", []) if is_driver(p) and pkg_installed(self.family, p)]
        if drivers:
            print("  " + t("kept_drivers", " ".join(drivers)))
        # Rounds: removing some packages (and the dependencies they pulled
        # in) can free others, e.g. the Vulkan loader once ffmpeg's
        # libplacebo is gone. A dry run removes nothing, so it shows one.
        safe = []
        for _ in range(1 if self.args.dry_run else 4):
            pkgs = [p for p in self.state.get("packages", []) if not is_driver(p) and pkg_installed(self.family, p)]
            safe = safe_set(self.family, pkgs)
            if not safe:
                break
            orphans = autoremovable() if self.family == "debian" else set()
            self.run(remove_command(self.family, safe), sudo=True)
            if self.family == "debian":
                # Only dependencies orphaned just now; older ones stay.
                new = sorted(autoremovable() - orphans)
                if new:
                    self.run(["apt-get", "remove", "-y"] + new, sudo=True)
        # (A dry run removed nothing: what it would have removed isn't kept.)
        used = [p for p in self.state.get("packages", []) if not is_driver(p) and pkg_installed(self.family, p)
                and not (self.args.dry_run and p in safe)]
        if used:
            print("  " + t("kept_used", " ".join(used)))

    def remove_rust(self):
        self.run([HOME / ".cargo" / "bin" / "rustup", "self", "uninstall", "-y"])

    def remove_rpmfusion(self):
        if quiet(["rpm", "-q", "ffmpeg"]) and not quiet(["rpm", "-q", "ffmpeg-free"]):
            self.run(["dnf", "swap", "-y", "ffmpeg", "ffmpeg-free", "--allowerasing"], sudo=True)
        # Other packages from RPM Fusion would stop getting updates.
        others = [line.split()[0] for line in output(["rpm", "-qa", "--qf", "%{NAME} %{VENDOR}\\n"]).splitlines()
                  if "RPM Fusion" in line and not line.startswith("rpmfusion-")]
        if others and not self.args.dry_run:
            print("  " + t("rpmfusion_kept", " ".join(others[:8])))
            return
        self.run(["dnf", "remove", "-y", "rpmfusion-free-release"], sudo=True)

    def save_state(self, chosen):
        """Forget what was removed, so a later run doesn't offer it again."""
        if self.args.dry_run or not self.state:
            return
        keys = {p.key for p in chosen}
        if "packages" in keys:
            self.state["packages"] = [p for p in self.state.get("packages", []) if pkg_installed(self.family, p)]
        if "rust" in keys:
            self.state["rustup"] = False
        if "rpmfusion" in keys and not quiet(["rpm", "-q", "rpmfusion-free-release"]):
            self.state["rpmfusion"] = False
        if "build" in keys:
            self.state["files"] = []
        if "system" in keys:
            self.state.pop("system", None)
        if not (self.state.get("packages") or self.state.get("rustup") or self.state.get("rpmfusion")
                or self.state.get("system") or self.state.get("files")):
            STATE.unlink(missing_ok=True)
            try:
                STATE.parent.rmdir()
            except OSError:
                pass
        else:
            STATE.write_text(json.dumps(self.state, indent=2))

def main():
    parser = argparse.ArgumentParser(description="Remove Freekstyle and what install.py added (see the top of this file).")
    parser.add_argument("-y", "--yes", action="store_true", help="don't ask: take each part's default (with --all: everything)")
    parser.add_argument("--all", action="store_true", help="default to removing everything, saves, packages and Rust included")
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

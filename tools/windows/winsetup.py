"""Windows install steps shared by tools/install.py (and mirrored by tools/windows/install.ps1).

Everything that touches the registry, the shell or PowerShell lives here and is only *called*
on Windows; the pure helpers (PATH merging, association tables, compiler selection) are
importable and testable anywhere. KEEP install.ps1 IN SYNC with ASSOCIATIONS, the PATH rules and
the shortcut: tools/test_install_windows.py checks that the ps1 mentions every key.
"""
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PureWindowsPath

try:
    import winreg
except ImportError:  # not Windows (tests inject a fake)
    winreg = None

PROGID_PROJECT = "SuperBrick.Project"
PROGID_LDRAW = "SuperBrick.LDraw"
EXES = ("sb.exe", "sb-dev.exe", "SuperBrick.exe")
CLASSES = r"Software\Classes"
SHORTCUT_NAME = "SuperBrick.lnk"


def default_prefix():
    return Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData/Local") / "Programs" / "SuperBrick"


def shortcut_path():
    appdata = os.environ.get("APPDATA") or str(Path.home() / "AppData/Roaming")
    return Path(appdata) / "Microsoft/Windows/Start Menu/Programs" / SHORTCUT_NAME


# ---- ray library -------------------------------------------------------------------------------

def ray_command(source, output, which=shutil.which):
    """Command (and its working directory hint) that builds studio_rays.dll; see docs/BUILD-STUDIO-RAYS.md.

    Prefers MSVC cl, then clang-cl, then mingw g++ (also the Linux cross compiler name).
    cl and clang-cl drop .obj/.lib/.exp files in the current directory, so run them in a scratch one."""
    source, output = str(source), str(output)
    if which("cl"):
        return ["cl", "/nologo", "/O2", "/LD", "/MT", "/std:c++17", "/EHsc", source, f"/Fe:{output}"]
    if which("clang-cl"):
        return ["clang-cl", "/O2", "/LD", "/std:c++17", "/EHsc", "/MT", source, f"/Fe:{output}"]
    for gxx in ("g++", "x86_64-w64-mingw32-g++"):
        if which(gxx):
            return [gxx, "-O3", "-std=c++17", "-shared", "-static", "-o", output, source]
    raise SystemExit("no C++ compiler for studio_rays.dll: open a Developer Command Prompt (cl), "
                     "or install clang-cl or mingw-w64 g++")


# ---- PATH --------------------------------------------------------------------------------------

def _norm(entry):
    return entry.strip().strip('"').rstrip("\\/").lower()


def merge_path(current, entry):
    """User PATH with ENTRY appended; None when it is already there (case-insensitive)."""
    parts = [p for p in current.split(";") if p.strip()]
    if any(_norm(p) == _norm(entry) for p in parts):
        return None
    return ";".join(parts + [str(entry)])


def remove_path(current, entry):
    """User PATH without ENTRY; None when it was not there."""
    parts = [p for p in current.split(";") if p.strip()]
    kept = [p for p in parts if _norm(p) != _norm(entry)]
    return None if len(kept) == len(parts) else ";".join(kept)


# ---- file associations -------------------------------------------------------------------------

def association_entries(bin_dir):
    """(subkey under HKCU, value name or "" for default, value) for every association value."""
    exe = str(PureWindowsPath(bin_dir) / "SuperBrick.exe")
    command = f'"{exe}" "%1"'
    c = CLASSES
    entries = [
        (rf"{c}\{PROGID_PROJECT}", "", "SuperBrick Project"),
        (rf"{c}\{PROGID_PROJECT}", "FriendlyTypeName", "SuperBrick Project"),
        (rf"{c}\{PROGID_PROJECT}\DefaultIcon", "", f"{exe},0"),
        (rf"{c}\{PROGID_PROJECT}\shell\open\command", "", command),
        (rf"{c}\.sb", "", PROGID_PROJECT),
        (rf"{c}\{PROGID_LDRAW}", "", "LDraw Model"),
        (rf"{c}\{PROGID_LDRAW}", "FriendlyTypeName", "LDraw Model"),
        (rf"{c}\{PROGID_LDRAW}\DefaultIcon", "", f"{exe},0"),
        (rf"{c}\{PROGID_LDRAW}\shell\open\command", "", command),
        (rf"{c}\Applications\SuperBrick.exe", "FriendlyAppName", "SuperBrick"),
        (rf"{c}\Applications\SuperBrick.exe\DefaultIcon", "", f"{exe},0"),
        (rf"{c}\Applications\SuperBrick.exe\shell\open\command", "", command),
    ]
    for ext in (".sb", ".ldr", ".mpd"):
        entries.append((rf"{c}\Applications\SuperBrick.exe\SupportedTypes", ext, ""))
    # .ldr/.mpd are offered ("Open with"), never taken over: only an OpenWithProgids value.
    for ext in (".ldr", ".mpd"):
        entries.append((rf"{c}\{ext}\OpenWithProgids", PROGID_LDRAW, ""))
    return entries


# Whole keys removed on uninstall (".sb" only when it still points at us; see unregister_associations).
OWNED_KEYS = [rf"{CLASSES}\{PROGID_PROJECT}", rf"{CLASSES}\{PROGID_LDRAW}", rf"{CLASSES}\Applications\SuperBrick.exe"]


def _require_winreg():
    if winreg is None:
        raise SystemExit("this step needs Windows (winreg is unavailable)")


def delete_tree(root, path):
    try:
        key = winreg.OpenKey(root, path, 0, winreg.KEY_ALL_ACCESS)
    except FileNotFoundError:
        return
    with key:
        while True:
            try:
                child = winreg.EnumKey(key, 0)
            except OSError:
                break
            delete_tree(root, rf"{path}\{child}")
    winreg.DeleteKey(root, path)


def register_associations(bin_dir):
    _require_winreg()
    for subkey, name, value in association_entries(bin_dir):
        with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, subkey, 0, winreg.KEY_SET_VALUE) as key:
            winreg.SetValueEx(key, name, 0, winreg.REG_SZ, value)
    notify_assoc_changed()


def unregister_associations():
    _require_winreg()
    hk = winreg.HKEY_CURRENT_USER
    for path in OWNED_KEYS:
        delete_tree(hk, path)
    try:  # .sb: remove only if we own the default
        with winreg.OpenKey(hk, rf"{CLASSES}\.sb", 0, winreg.KEY_READ) as key:
            owned = winreg.QueryValueEx(key, "")[0] == PROGID_PROJECT
        if owned:
            delete_tree(hk, rf"{CLASSES}\.sb")
    except FileNotFoundError:
        pass
    for ext in (".ldr", ".mpd"):
        try:
            with winreg.OpenKey(hk, rf"{CLASSES}\{ext}\OpenWithProgids", 0, winreg.KEY_SET_VALUE) as key:
                winreg.DeleteValue(key, PROGID_LDRAW)
        except FileNotFoundError:
            pass
    notify_assoc_changed()


def notify_assoc_changed():
    import ctypes
    ctypes.windll.shell32.SHChangeNotify(0x08000000, 0, None, None)  # SHCNE_ASSOCCHANGED


# ---- environment (PATH, SB_PYTHON) -------------------------------------------------------------

def broadcast_environment_change():
    import ctypes
    from ctypes import wintypes
    result = wintypes.DWORD()
    send = ctypes.windll.user32.SendMessageTimeoutW
    send.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, ctypes.c_wchar_p,
                     wintypes.UINT, wintypes.UINT, ctypes.POINTER(wintypes.DWORD)]
    send(0xFFFF, 0x1A, 0, "Environment", 0x2, 5000, ctypes.byref(result))  # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG


def _read_user_env(name):
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_READ) as key:
            return winreg.QueryValueEx(key, name)[0]
    except FileNotFoundError:
        return ""


def _write_user_env(name, value, kind=winreg.REG_EXPAND_SZ if winreg else 2):
    with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_SET_VALUE) as key:
        winreg.SetValueEx(key, name, 0, kind, value)


def add_to_user_path(bin_dir):
    _require_winreg()
    merged = merge_path(_read_user_env("Path"), str(bin_dir))
    if merged is not None:
        _write_user_env("Path", merged)
        broadcast_environment_change()
    return merged is not None


def remove_from_user_path(bin_dir):
    _require_winreg()
    kept = remove_path(_read_user_env("Path"), str(bin_dir))
    if kept is not None:
        _write_user_env("Path", kept)
        broadcast_environment_change()
    return kept is not None


def set_user_env(name, value):
    _require_winreg()
    _write_user_env(name, value, winreg.REG_SZ)
    broadcast_environment_change()


def unset_user_env(name):
    _require_winreg()
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_SET_VALUE) as key:
            winreg.DeleteValue(key, name)
    except FileNotFoundError:
        return
    broadcast_environment_change()


# ---- Start Menu shortcut -----------------------------------------------------------------------

SHORTCUT_PS = ("$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:SB_LNK);"
               "$s.TargetPath=$env:SB_TARGET;$s.WorkingDirectory=$env:SB_DIR;"
               "$s.IconLocation=$env:SB_TARGET+',0';$s.Description='SuperBrick';$s.Save()")


def create_shortcut(bin_dir):
    link = shortcut_path()
    link.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, SB_LNK=str(link), SB_TARGET=str(Path(bin_dir) / "SuperBrick.exe"), SB_DIR=str(bin_dir))
    subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", SHORTCUT_PS], env=env, check=True)
    return link


def remove_shortcut():
    link = shortcut_path()
    try:
        link.unlink()
    except FileNotFoundError:
        pass


# ---- files -------------------------------------------------------------------------------------

def replace_exe(source, target):
    """Copy SOURCE to TARGET even if TARGET is running.

    Windows can rename a running exe but not overwrite it, so move it to TARGET.old first
    (after deleting a stale .old; failures there are ignored). The new copy is written to a
    temporary file and renamed into place."""
    source, target = Path(source), Path(target)
    target.parent.mkdir(parents=True, exist_ok=True)
    old = target.with_name(target.name + ".old")
    if target.exists():
        try:
            old.unlink()
        except OSError:
            pass
        try:
            os.replace(target, old)
        except OSError:
            pass  # not running or not renamable: the replace below reports the real problem
    fd, temporary = tempfile.mkstemp(prefix=".sb-install-", dir=target.parent)
    os.close(fd)
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    try:  # best effort: fails while the old exe is still running
        old.unlink()
    except OSError:
        pass


def install_windows(release_dir, share_source, prefix, pin_python=None):
    """Copy the three exes and the staged share into PREFIX and register everything for the user."""
    prefix = Path(prefix)
    bin_dir = prefix / "bin"
    for name in EXES:
        replace_exe(Path(release_dir) / name, bin_dir / name)
    if share_source is not None and Path(share_source).resolve() != (prefix / "share/superbrick").resolve():
        shutil.copytree(share_source, prefix / "share/superbrick", dirs_exist_ok=True)
    register_associations(bin_dir)
    path_changed = add_to_user_path(bin_dir)
    link = create_shortcut(bin_dir)
    if pin_python:
        set_user_env("SB_PYTHON", str(pin_python))
    return bin_dir, link, path_changed


def uninstall_windows(prefix):
    prefix = Path(prefix)
    unregister_associations()
    remove_shortcut()
    remove_from_user_path(prefix / "bin")
    if prefix.exists():
        for exe in (prefix / "bin").glob("*.exe"):  # a running exe cannot be deleted but can be renamed
            try:
                os.replace(exe, exe.with_name(exe.name + ".old"))
            except OSError:
                pass
        shutil.rmtree(prefix, ignore_errors=True)
        if prefix.exists():
            print(f"Some files in {prefix} are still in use; delete the folder after closing SuperBrick.",
                  file=sys.stderr)

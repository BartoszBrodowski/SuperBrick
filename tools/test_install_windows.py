#!/usr/bin/env python3
"""Linux-runnable tests for the Windows install helpers (winreg is faked): python3 tools/test_install_windows.py"""
import re
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import install  # noqa: E402
import release  # noqa: E402
import winsetup  # noqa: E402


class FakeWinreg:
    """Just enough of winreg: a dict of keys -> {name: (value, kind)}."""
    HKEY_CURRENT_USER = "HKCU"
    KEY_SET_VALUE = KEY_READ = KEY_ALL_ACCESS = 0
    REG_SZ, REG_EXPAND_SZ = 1, 2

    def __init__(self):
        self.keys = {}

    class _K:
        def __init__(self, fake, path):
            self.fake, self.path = fake, path

        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

    def CreateKeyEx(self, root, path, res, access):
        parts = path.lower().split("\\")
        for i in range(1, len(parts) + 1):  # like the registry, create every ancestor
            self.keys.setdefault("\\".join(parts[:i]), {})
        return self._K(self, path.lower())

    def OpenKey(self, root, path, res=0, access=0):
        if path.lower() not in self.keys:
            raise FileNotFoundError(path)
        return self._K(self, path.lower())

    def SetValueEx(self, key, name, res, kind, value):
        self.keys[key.path][name] = (value, kind)

    def QueryValueEx(self, key, name):
        if name not in self.keys[key.path]:
            raise FileNotFoundError(name)
        return self.keys[key.path][name]

    def DeleteValue(self, key, name):
        if name not in self.keys[key.path]:
            raise FileNotFoundError(name)
        del self.keys[key.path][name]

    def EnumKey(self, key, i):
        kids = sorted({k[len(key.path) + 1:].split("\\")[0] for k in self.keys if k.startswith(key.path + "\\")})
        if i >= len(kids):
            raise OSError
        return kids[i]

    def DeleteKey(self, root, path):
        del self.keys[path.lower()]

    def QueryInfoKey(self, key):
        kids = {k for k in self.keys if k.startswith(key.path + "\\")}
        return len(kids), len(self.keys[key.path]), 0


class PathTests(unittest.TestCase):
    def test_merge(self):
        self.assertEqual(winsetup.merge_path("C:\\a;C:\\b", "C:\\x\\bin"), "C:\\a;C:\\b;C:\\x\\bin")
        self.assertEqual(winsetup.merge_path("", "C:\\x\\bin"), "C:\\x\\bin")
        self.assertIsNone(winsetup.merge_path("C:\\a;c:\\X\\BIN\\", "C:\\x\\bin"))
        self.assertEqual(winsetup.merge_path("C:\\a;;", "D:\\b"), "C:\\a;D:\\b")

    def test_remove(self):
        self.assertEqual(winsetup.remove_path("C:\\a;C:\\X\\bin;C:\\b", "c:\\x\\bin"), "C:\\a;C:\\b")
        self.assertIsNone(winsetup.remove_path("C:\\a", "C:\\x\\bin"))


class NameTests(unittest.TestCase):
    def test_library_names(self):
        self.assertEqual(install.ray_library_name("win32"), "studio_rays.dll")
        self.assertEqual(install.ray_library_name("darwin"), "studio_rays.dylib")
        self.assertEqual(install.ray_library_name("linux"), "studio_rays.so")

    def test_compiler_choice(self):
        has = lambda *names: (lambda n: n if n in names else None)
        self.assertEqual(winsetup.ray_command("a.cpp", "o.dll", has("cl", "g++"))[0], "cl")
        self.assertEqual(winsetup.ray_command("a.cpp", "o.dll", has("clang-cl", "g++"))[0], "clang-cl")
        self.assertEqual(winsetup.ray_command("a.cpp", "o.dll", has("g++"))[0], "g++")
        self.assertIn("-static", winsetup.ray_command("a.cpp", "o.dll", has("g++")))
        with self.assertRaises(SystemExit):
            winsetup.ray_command("a.cpp", "o.dll", has())

    def test_foreign_dlls(self):
        self.assertEqual(release.foreign_dlls(["KERNEL32.dll", "api-ms-win-core-x.dll", "D3D12.dll"]), [])
        self.assertEqual(release.foreign_dlls(["VCRUNTIME140.dll", "user32.dll"]), ["VCRUNTIME140.dll"])


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakeWinreg()
        winsetup.winreg = self.fake
        winsetup.notify_assoc_changed = lambda: None
        winsetup.broadcast_environment_change = lambda: None

    def test_associations_roundtrip(self):
        winsetup.register_associations("C:\\P\\bin")
        k = self.fake.keys
        self.assertEqual(k[r"software\classes\.sb"][""][0], "SuperBrick.Project")
        self.assertEqual(k[r"software\classes\superbrick.project\shell\open\command"][""][0],
                         '"C:\\P\\bin\\SuperBrick.exe" "%1"')
        self.assertNotIn("", k.get(r"software\classes\.ldr", {}))  # default not taken over
        self.assertIn("SuperBrick.LDraw", k[r"software\classes\.mpd\openwithprogids"])
        winsetup.unregister_associations()
        self.assertFalse([p for p in k if "superbrick" in p or p.endswith(".sb")])
        self.assertNotIn("SuperBrick.LDraw", k[r"software\classes\.ldr\openwithprogids"])

    def test_previous_owner_is_restored_and_other_entries_kept(self):
        self.fake.CreateKeyEx("HKCU", r"Software\Classes\.sb\OpenWithProgids", 0, 0)
        self.fake.keys[r"software\classes\.sb"][""] = ("Other.App", 1)
        self.fake.keys[r"software\classes\.sb\openwithprogids"]["Other.App"] = ("", 1)
        winsetup.register_associations("C:\\P\\bin")
        self.assertEqual(self.fake.keys[r"software\classes\.sb"][""][0], "SuperBrick.Project")
        winsetup.unregister_associations()
        self.assertEqual(self.fake.keys[r"software\classes\.sb"], {"": ("Other.App", 1)})
        self.assertIn("Other.App", self.fake.keys[r"software\classes\.sb\openwithprogids"])

    def test_sb_kept_if_not_ours(self):
        winsetup.register_associations("C:\\P\\bin")
        self.fake.keys[r"software\classes\.sb"][""] = ("Other.App", 1)
        winsetup.unregister_associations()
        self.assertIn(r"software\classes\.sb", self.fake.keys)

    def test_user_path(self):
        self.assertTrue(winsetup.add_to_user_path("C:\\P\\bin"))
        self.assertFalse(winsetup.add_to_user_path("c:\\p\\BIN"))
        self.assertEqual(self.fake.keys["environment"]["Path"], ("C:\\P\\bin", 2))
        self.assertTrue(winsetup.remove_from_user_path("C:\\P\\bin"))

    def test_ps1_in_sync(self):
        ps1 = (HERE / "windows/install.ps1").read_text()
        normalized = ps1.replace("$e\\OpenWithProgids", ".ldr\\OpenWithProgids")
        for subkey, name, value in winsetup.association_entries("C:\\P\\bin"):
            tail = subkey[len(winsetup.CLASSES) + 1:]
            if tail.endswith("OpenWithProgids"):
                tail = "OpenWithProgids"
            self.assertIn(tail, normalized)
            if name:
                self.assertIn(name, ps1)
        for needle in ("HKCU:\\Environment", "ExpandString", "0x1A", "0x08000000", "SuperBrick.lnk", ".old"):
            self.assertIn(needle, ps1)


class FileTests(unittest.TestCase):
    def test_replace_exe_moves_old_aside(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "new.exe").write_text("new")
            (d / "bin").mkdir()
            (d / "bin/sb.exe").write_text("old")
            (d / "bin/sb.exe.old").write_text("stale")
            winsetup.replace_exe(d / "new.exe", d / "bin/sb.exe")
            self.assertEqual((d / "bin/sb.exe").read_text(), "new")
            self.assertFalse((d / "bin/sb.exe.old").exists())
            winsetup.replace_exe(d / "new.exe", d / "fresh/sb.exe")
            self.assertEqual((d / "fresh/sb.exe").read_text(), "new")


if __name__ == "__main__":
    unittest.main()

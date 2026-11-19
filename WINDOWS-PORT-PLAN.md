# SuperBrick Windows port plan

Goal: build and run SuperBrick (`sb` GUI, CLI, MCP server, Python helpers, installer) on
Windows 10/11 x64 with the same functionality and UI. macOS behaviour must not change: every
change is either `cfg(windows)`-gated or behaviour-neutral on macOS.

## What I found

The Rust UI and rendering stack (eframe/egui, egui_dock, wgpu, rfd, rusqlite bundled, memmap2,
tokenizers/onig) is already cross-platform. A Windows cross-check
(`cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`) fails in only
**one file**, `app/src/autosave.rs` (it uses `std::os::unix` permissions). Everything else is a
**runtime** problem. The code assumes macOS paths and tools and Unix process behaviour.

| # | Area | Where | Problem on Windows | Severity |
|---|---|---|---|---|
| 1 | Compile error | `app/src/autosave.rs:81-93` | `std::os::unix::fs::{OpenOptionsExt, PermissionsExt}` | blocks build |
| 2 | App data paths | `crates/ldraw/src/runtime.rs:33-49`, `app/src/autosave.rs:10-19` (and `project.rs:385` prefs, which uses the autosave dir) | `$HOME/Library/Application Support`, `$HOME/Library/Caches`; `HOME` is usually unset on Windows | broken |
| 3 | `~` expansion and display | `crates/ldraw/src/library.rs:71`, `app/src/home.rs:174` | uses `HOME` | minor |
| 4 | Default parts library | `crates/ldraw/src/library.rs:19` | `/Applications/LeoCAD.app/...` | LeoCAD fallback lost |
| 5 | Python discovery | `runtime.rs:92-117` | Homebrew/framework paths, `/usr/bin/xcode-select`, `bin/python3` layout; the Microsoft Store `python.exe` alias is a stub, like the macOS `/usr/bin/python3` one | broken |
| 6 | Managed Python install | `runtime.rs:21-24, 70-72, 243-276` | macOS aarch64 tarball and SHA; `/usr/bin/curl`, `/usr/bin/shasum`, `/usr/bin/tar`; `python/bin/python3` (Windows build: `python\python.exe`) | broken |
| 7 | **Helper scripts passed with `python -c`** | `crates/engine/src/connectors.rs:82-85` (29 KB), `app/src/publication.rs:188, 316` (82 KB), `crates/engine/tests/connectors.rs:83,126` | the Windows command line is limited to 32,767 chars, so `publication.py` cannot start at all and `connectors.py` is close to the limit | broken: Connect, Hinge, instructions, studio |
| 8 | Console windows | every `Command::spawn` (Python, curl, tar, `sb` job children) | a GUI process starting console children opens a black window for each one unless `CREATE_NO_WINDOW` is set | UX regression |
| 9 | GUI vs. console subsystem | `app/src/main.rs` | the same `sb` binary is the GUI, the CLI and the stdio MCP server. A console-subsystem exe opens a terminal when launched from Explorer; a windows-subsystem exe loses stdio | needs design |
| 10 | Open/reveal files | `app/src/publication.rs:516,522`, `app/src/home.rs:284` | `open`, `open -R` | broken buttons |
| 11 | Cancel detection | `app/src/publication.rs:353-357` | uses `status.code() == None` (killed by a signal) to mean "cancelled"; on Windows `kill()` gives exit code 1, so Cancel shows as a failure | wrong message |
| 12 | Process liveness | `crates/store/src/lib.rs:846-864` (`#[cfg(not(unix))]` → always `true`), `crates/ops/src/tools/publish.rs:53-71` (`kill -0`) | project leases are held until they go stale; publish jobs from earlier processes read as dead | degraded |
| 13 | Finding the `sb` binary | `crates/ops/src/tools/publish.rs:39` | compares the file name to `"sb"` (it is `sb.exe` on Windows) | publish uses PATH instead |
| 14 | MCP hot reload | `crates/ops/src/front.rs:635-652, 929-940` | needs Unix `exec` and an fd dup; disabled on non-Unix. Also a running `.exe` cannot be overwritten by a reinstall | dev feature lost |
| 15 | Native ray library | `app/src/studio_rays.cpp:46`, `app/src/publication.py:1290`, `tools/install.py:103-109` | built as `.dylib` with `clang++ -dynamiclib`; no `__declspec(dllexport)`; the `.dylib` name is hard-coded | slow pure-Python fallback |
| 16 | Opening files from Explorer | `app/src/open_files.rs` (macOS Apple Events only) | Windows passes the file as `argv` (already handled by `parse_args`), but a second double-click starts a **second window** instead of going to the running one | small behaviour difference |
| 17 | Install and release | `tools/install.py`, `tools/release.py` | `.app` bundle, Info.plist, `codesign`, `lsregister`, `otool`, DMG, `/bin/sh` launcher, `~/.local/bin` | no Windows install |
| 18 | App/exe icon | `app/assets/SuperBrick.icns` | no `.ico` and no icon resource in the exe (the window icon already works through `brand::icon()`) | cosmetic |
| 19 | macOS wording in the UI | `app/src/panels.rs:951` "⌘S", `app/src/app.rs:1144` "⌘K", `home.rs:283` "Show in Finder", `setup.rs:275` "on your Mac" | wrong labels on Windows | cosmetic |
| 20 | Unix-only tests | `app/src/publication.rs:611` (`/bin/sh`), `crates/store/tests/store.rs:43` (`HOME`) | fail on Windows CI | tests |
| 21 | **External Python helpers** | `../bench-next/*.py`, `../data-next/...` (copied by `install.py`, **not in this repo**) | not auditable here; they may use `fork`, `signal`, `/tmp` or hard-coded `.dylib` names | unknown risk |
| 22 | Dead Unix-only crate | `crates/agent` (Unix sockets, `libc::geteuid`) | not a workspace member, so it is not built | ignore, or gate with `cfg(unix)` |

Things that need no change: keyboard shortcuts (they already use `Modifiers::COMMAND`, which
is Ctrl on Windows), wgpu (DX12/Vulkan are picked automatically; WGSL shaders are portable),
rfd dialogs, SQLite/WAL `.sb` files, the LDraw zip (`library.rs:51` already turns `\` into
`/`), drag and drop, and the window icon.

## Design decisions (fixed up front so parallel agents agree)

1. **One platform module, `crates/ldraw/src/platform.rs`**. Every crate already depends on
   `ldraw`. All OS-specific code goes here, behind a small API (see T0). Other tasks call it and
   add no `cfg` blocks of their own, except for trivial gates.
2. **Windows directories**: support dir is `%APPDATA%\SuperBrick` (prefs, recovery, LDraw zip,
   managed Python). Cache dir is `%LOCALAPPDATA%\SuperBrick\Cache`. Home is `%USERPROFILE%`.
   The existing `SB_*` / `CAD_*` environment overrides keep working.
3. **Two executables on Windows**: `sb.exe` (console subsystem, the existing binary: CLI, MCP,
   and the GUI when started from a terminal) and `SuperBrick.exe` (a tiny `windows`-subsystem
   launcher that runs `sb.exe` next to it with the same arguments and `CREATE_NO_WINDOW`). This
   mirrors the macOS `bin/sb` → app-bundle launcher. Shortcuts and file associations point at
   `SuperBrick.exe`.
4. **Helper scripts are written to disk, not passed with `-c`**: write the embedded script once
   to `cache_dir()/helpers/<name>-<fnv hash>.py` and run `python -u <file> …`. Do this on all
   platforms (no behaviour change on macOS, and one code path).
5. **Downloads keep using OS tools** (the "no network code in the app" design). On Windows use
   `%SystemRoot%\System32\curl.exe` and `tar.exe` (present since Windows 10 1803). Replace
   `shasum` with the `sha2` crate on every platform.
6. **Native ray library**: `studio_rays.dll` on Windows, built with MSVC
   (`cl /O2 /LD /std:c++17 /EHsc`) or `clang-cl`. Python picks the suffix from `sys.platform`.
7. **Target**: `x86_64-pc-windows-msvc` first. `aarch64-pc-windows-msvc` is a follow-up.
8. **Install layout** (it matches `ldraw::installed_share`, which looks in `bin/../share/superbrick`):
   `%LOCALAPPDATA%\Programs\SuperBrick\bin\{sb.exe, sb-dev.exe, SuperBrick.exe}` and
   `…\share\superbrick\{search,connectors}`. `bin` is added to the user PATH.

## Task schedule

```
Wave 0:  T0 platform module                          (one agent, blocks everything)
Wave 1:  T1 paths ─┬─ T2 python ─┬─ T3 helpers ─┬─ T4 liveness/MCP ─┬─ T5 native lib ─┬─ T7 UI text
                   (parallel; disjoint file ownership, listed per task)
Wave 2:  T6 packaging (needs T5's build command)   T8 single instance (optional)   T10 docs
Wave 3:  T9 Windows CI and end-to-end verification (needs everything)
External: T11 audit bench-next/data-next (needs those folders from the owner)
```

Each agent works in its own git worktree or branch and touches **only the files it owns**. If
it needs something from a file it does not own, it notes that in its report and does not edit
the file.

### Verification every agent runs (Linux container)

```bash
sudo apt-get install -y gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64   # once
rustup target add x86_64-pc-windows-gnu                              # once
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu # Windows compiles
cargo check --workspace --all-targets                                # host still compiles
cargo test -p <crates you touched>                                   # no regressions
```

On Linux the `cfg(unix)` path stands in for macOS. Real Windows runtime checks happen in T9.

---

### T0: Platform module (Wave 0)

**Owns:** `crates/ldraw/src/platform.rs` (new), `crates/ldraw/src/lib.rs` (`pub mod platform;`),
`crates/ldraw/Cargo.toml`, `crates/ldraw/src/runtime.rs` (**only** `home`, `support_dir`, `cache_dir`).

Add `windows-sys` (features: `Win32_Foundation`, `Win32_System_Threading`, `Win32_UI_Shell`,
`Win32_System_Console`) under `[target.'cfg(windows)'.dependencies]`. API:

- `home_dir() -> PathBuf`: `HOME` on Unix; `USERPROFILE` on Windows.
- `support_root() -> PathBuf`: `~/Library/Application Support` on macOS; `%APPDATA%` on Windows.
- `cache_root() -> PathBuf`: `~/Library/Caches` on macOS; `%LOCALAPPDATA%` on Windows
  (`support_dir`/`cache_dir` then add `SuperBrick` and, on Windows, `SuperBrick\Cache`).
- `open_path(&Path) -> io::Result<()>`: `open` on macOS; `ShellExecuteW("open")` on Windows.
- `reveal_path(&Path)`: `open -R` on macOS; `explorer.exe /select,"<path>"` on Windows.
- `no_window(&mut Command) -> &mut Command`: no-op on Unix; `creation_flags(CREATE_NO_WINDOW)` on Windows.
- `pid_alive(u32) -> bool`: the current `kill -0` on Unix; `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
  plus `GetExitCodeProcess == STILL_ACTIVE` on Windows.
- `exe_name(stem) -> String`: `"sb"` / `"sb.exe"`.
- `shared_lib_name(stem)`: `lib.dylib` / `.so` / `.dll`.
- `file_manager_name() -> &'static str`: `"Finder"` / `"Explorer"` / `"file manager"`.

Rewrite `runtime::home/support_dir/cache_dir` to use these. Unit-test the pure functions.
**Done when** both `cargo check` targets pass and the macOS-path tests are unchanged.

### T1: App-side paths (Wave 1)

**Owns:** `app/src/autosave.rs`, `app/src/project.rs`, `crates/ldraw/src/library.rs`, `app/src/home.rs`.

- `autosave.rs`: build the directory from `ldraw::runtime::support_dir()` (keep the old
  `cad/recovery` migration on macOS). Gate the `0o700`/`0o600` permission code with
  `#[cfg(unix)]`; on Windows use plain `create_new(true)`. This fixes the only compile error.
- `library.rs`: `expand_home` uses `platform::home_dir()`. `DEFAULT_ZIP` becomes a function
  that returns `C:\Program Files\LeoCAD\library.bin` (and `%ProgramFiles(x86)%`) on Windows.
  Update the caller in `runtime::library_zip` with a one-line change (coordinate with T2: T1
  edits only the `DEFAULT_ZIP` reference line).
- `home.rs`: `tilde()` uses `platform::home_dir()` and handles separators case-insensitively
  on Windows. "Show in Finder" calls `platform::reveal_path` and its label is
  `format!("Show in {}", platform::file_manager_name())`.
- Check that `project.rs` prefs still resolve under the support dir.

### T2: Python discovery and managed install (Wave 1)

**Owns:** `crates/ldraw/src/runtime.rs` (everything except T0's three functions),
`crates/ldraw/Cargo.toml` (add `sha2`; rebase on T0).

- `managed_python()`: `python\python.exe` on Windows.
- `find_python()` on Windows, in order: `SB_PYTHON`, managed, then the
  `py -3 -c "import sys;print(sys.executable)"` result, then
  `%LOCALAPPDATA%\Programs\Python\Python3*\python.exe` (newest first),
  `%USERPROFILE%\{miniforge3,miniconda3,anaconda3}\python.exe`, `%USERPROFILE%\scoop\shims\python.exe`,
  then `python.exe` found on `PATH`. **Skip anything under `\WindowsApps\`** (the Store stub).
  Keep the macOS list unchanged under `cfg(target_os = "macos")`.
- `PYTHON_URL`/`PYTHON_BYTES`/`PYTHON_SHA256`: add a `cfg` table per target
  (`x86_64-pc-windows-msvc-install_only_stripped.tar.gz` from the same python-build-standalone
  release `20261110`, CPython 3.12.16). Get the size and SHA-256 by downloading the file once.
- `download()`/`install_python()`: on Windows use `%SystemRoot%\System32\curl.exe` and `tar.exe`.
  Replace `sha256()` with `sha2` on every platform. Wrap every `Command` in `platform::no_window`.
- If `rename` of the staging folder fails because a file is in use, retry a few times
  (antivirus scanners hold new files briefly).
- Tests: the existing `python_base`/`usable` tests, plus a Windows-gated test that a `WindowsApps` path is rejected.

### T3: Starting helper processes (Wave 1)

**Owns:** `crates/engine/src/connectors.rs`, `crates/engine/tests/connectors.rs`, `app/src/publication.rs`.

- Add `ldraw::runtime::helper_script(name, source) -> Result<PathBuf>` (T3 may add **this one
  function** at the end of `runtime.rs`; tell T2). It writes the file atomically to
  `cache_dir()/helpers/<name>-<fnv1a64 hex>.py` if it is missing.
- Replace the `-c include_str!(…)` calls (`connectors.rs:82-85`, `publication.rs:188, 316`,
  `tests/connectors.rs:83,126`) with that file path. Arguments after it stay the same.
- Apply `platform::no_window` to every spawn in these files.
- Cancel: add an `Arc<AtomicBool> cancelled` that the Cancel button and `Drop` set before
  `kill()`. Report "Job stopped/cancelled." when it is set, not when `code() == None`.
- `publication.rs:516,522`: use `platform::open_path`.
- Gate the `/bin/sh` test (`publication.rs:611`) with `#[cfg(unix)]` and add a `cmd /C` twin under `#[cfg(windows)]`.

### T4: Process liveness, publish jobs, MCP reload (Wave 1)

**Owns:** `crates/store/src/lib.rs` (`pid_alive` only), `crates/ops/src/tools/publish.rs`, `crates/ops/src/front.rs`, `crates/store/tests/store.rs`.

- `store::pid_alive` and `publish::alive` call `ldraw::platform::pid_alive`.
- `publish::sb_binary`: compare `file_stem()` with `"sb"`; the fallback is `platform::exe_name("sb")`.
  Apply `no_window` to the job spawn.
- MCP reload (`front.rs`): on Windows, keep hot reload off for this task (the current fallback
  already serves without it) and make sure the server never tries to `exec`. If time allows,
  add a Windows version: spawn the new exe with the handoff file, pipe the rest of stdin to it,
  copy its stdout, and exit with its code. Document that on Windows a reinstall must rename the
  running `sb.exe` away first (T6 does this).
- `store.rs:43` test: use `platform::home_dir()`, or skip when the fixture is missing.

### T5: Native ray library (Wave 1)

**Owns:** `app/src/studio_rays.cpp`, `app/src/publication.py`.

- Add `#if defined(_WIN32)  #define CAD_EXPORT extern "C" __declspec(dllexport)` (otherwise
  `extern "C" __attribute__((visibility("default")))`) and mark the exported functions
  (`cad_rasterize`, `cad_rays_build`, `cad_rays_cast`, `cad_rays_inside`, `cad_rays_free`).
  `long long` is already used for 64-bit ints, so the Windows ABI is safe.
- `publication.py:1290`: choose `studio_rays.dll` / `.dylib` / `.so` from `sys.platform`. On
  Windows also call `os.add_dll_directory(dirname)` before `ctypes.CDLL`.
- Check every path in `publication.py` for `/`-joined strings, `/tmp`, `os.fork` and signals,
  and fix any you find. Make HTML/file output paths use `Path`.
- Document the exact build commands for T6 (MSVC `cl /O2 /LD /std:c++17 /EHsc studio_rays.cpp /Fe:studio_rays.dll`;
  mingw `x86_64-w64-mingw32-g++ -O3 -std=c++17 -shared -static -o studio_rays.dll`). Verify
  with mingw that the DLL exports the symbols (`x86_64-w64-mingw32-objdump -p`).

### T7: Platform-correct UI wording (Wave 1)

**Owns:** `app/src/panels.rs`, `app/src/app.rs` (string literals only), `app/src/setup.rs`.

Only text changes, no layout or behaviour changes. Format shortcut hints with
`ctx.format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S))`, which shows ⌘S on
macOS and Ctrl+S on Windows. Do the same for ⌘K. Change "on your Mac" to "on your computer"
on Windows (`cfg!`), and check `setup.rs` for any other macOS paths shown to the user.

### T6: Packaging and install (Wave 2, needs T5)

**Owns:** `tools/install.py`, `tools/release.py`, `Cargo.toml` (workspace members),
`crates/launcher/` (new), `app/build.rs` (new), `app/Cargo.toml`, `app/assets/SuperBrick.ico` (new).

- `crates/launcher`: `#![windows_subsystem = "windows"]` bin named `SuperBrick` that runs
  `sb.exe` from its own folder with the same args and `CREATE_NO_WINDOW`, then exits. It does
  not wait. On non-Windows it is an empty `main` (or `cfg`-gated out of the build).
- Icon: generate `SuperBrick.ico` (16/32/64/128/256) from `app/assets/icon-*.png` with Pillow;
  commit it. `app/build.rs` embeds it with `winresource` (build-dependency, only when
  `CARGO_CFG_TARGET_OS == "windows"`). Do the same for the launcher.
- `install.py`: `sys.platform == "win32"` branch. Default prefix
  `%LOCALAPPDATA%\Programs\SuperBrick`. Build, then `stage_share` (replace the `.dylib` build
  step with T5's DLL command), and copy `sb.exe`, `sb-dev.exe` and `SuperBrick.exe`. If `sb.exe`
  is running, rename it to `sb.exe.old` first. Add `bin` to the user PATH (`HKCU\Environment`,
  then broadcast `WM_SETTINGCHANGE`). Register file types under `HKCU\Software\Classes`:
  `SuperBrick.Project` owns `.sb`; `.ldr`/`.mpd` get `OpenWithProgids`. Command:
  `"…\SuperBrick.exe" "%1"`, with the icon. Create a Start Menu shortcut. `--app` keeps meaning macOS only.
- `release.py`: Windows branch that builds `dist/SuperBrick-VERSION-Windows-x64.zip` (the
  layout above plus `install.ps1`, or an Inno Setup `.iss` if `iscc` is present) with a Read me
  adapted from `READ_ME` (SmartScreen "More info → Run anyway" instead of Gatekeeper).
  Replace the `otool` check with a `dumpbin /dependents` check (system DLLs plus the VC runtime;
  link the CRT statically with `-C target-feature=+crt-static` to avoid shipping it).

### T8: Single instance (Wave 2, optional, needs T0)

**Owns:** `app/src/open_files.rs`, `app/src/main.rs`.

This matches macOS, where opening a file while SuperBrick runs sends it to the running app. On
Windows, at GUI start, try to connect to the named pipe `\\.\pipe\SuperBrick-<user SID or name>`.
If a server answers, send the paths, bring that window forward
(`AllowSetForegroundWindow`), and exit. Otherwise start a pipe-server thread that calls the
existing `push(paths)`. Only GUI launches with a file take part; CLI/MCP subcommands never do.

### T10: Docs (Wave 2)

**Owns:** `README.md`, `docs/SETUP.md`, `docs/FEATURES.md`.
Add Windows download/first-launch notes, the Windows rows of the "Where files live" table, and build steps.

### T9: Windows CI and end-to-end check (Wave 3)

**Owns:** `.github/workflows/windows.yml` (new), plus fixes to anything still broken (coordinate with the owners).

- Run `cargo build --release` and `cargo test --workspace` on `windows-latest` (MSVC). Cache
  `target`. Install Python 3.12 with numpy, pillow and scipy for the connector tests.
- Run the smoke scripts (`tools/smoke*.py`). Port their `~/.local/bin/cad` paths and
  `CAD_SOCKET` assumptions, or mark them macOS-only.
- Manual checklist on a real Windows machine (a GitHub runner cannot show the GUI): first-run
  setup downloads the library and Python; open `.sb`/`.ldr` by double-click; Connect/Hinge
  (connector backend); Instructions and Studio render, including Cancel; "Open printable
  instruction document"; "Show in Explorer"; recovery autosave; `sb mcp --user claude` from
  PowerShell with Claude Code; a second instance shows read-only lease behaviour; no console
  windows flash during any of this.

### T11: External helper audit (blocked on the owner)

`tools/install.py` copies `../bench-next/{physics,snaps,ldraw,raster,render,validate,scene,orient}.py`
and `../data-next/ldcad_shadow/...`. These are not in this repository. `connectors.py` and
`publication.py` import them. They need the same audit as T5 (`fork`, `signal`, `/tmp`,
`.dylib`, `multiprocessing` start method, text-mode newline handling on pipes). Assign this when
those folders are available.

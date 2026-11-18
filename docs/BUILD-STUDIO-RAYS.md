# Building the native ray library

Source: `app/src/studio_rays.cpp` (C++17, no dependencies beyond the standard library and threads).
Exports (C ABI, 64-bit ints are `long long`): `cad_rasterize`, `cad_rays_build`, `cad_rays_cast`,
`cad_rays_inside`, `cad_rays_free`.

Install location: `share/superbrick/connectors/` next to the other connector files. `publication.py`
finds it as `studio_rays.<ext>` beside the bench directory (or via `CAD_RAY_LIBRARY`).
`<ext>` is `dylib` on macOS, `dll` on Windows, `so` elsewhere.

## macOS (existing)

    clang++ -O3 -std=c++17 -dynamiclib -pthread -install_name @loader_path/studio_rays.dylib app/src/studio_rays.cpp -o studio_rays.dylib

## Windows, MSVC (from a Developer Command Prompt, x64)

    cl /O2 /LD /std:c++17 /EHsc app\src\studio_rays.cpp /Fe:studio_rays.dll

Add `/MT` to link the runtime statically so no VC++ redistributable is needed.

## Windows, clang-cl

    clang-cl /O2 /LD /std:c++17 /EHsc /MT app\src\studio_rays.cpp /Fe:studio_rays.dll

## Windows, mingw-w64 (cross-compile from Linux)

    x86_64-w64-mingw32-g++ -O3 -std=c++17 -shared -static -o studio_rays.dll app/src/studio_rays.cpp

Verify exports: `x86_64-w64-mingw32-objdump -p studio_rays.dll | grep cad_`

## Linux (host tests)

    g++ -O3 -std=c++17 -shared -fPIC -pthread -o studio_rays.so app/src/studio_rays.cpp

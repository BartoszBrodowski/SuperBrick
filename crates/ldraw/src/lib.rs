//! LDraw for the SuperBrick workspace: library access, colours, geometry flattening and lossless model I/O.
//!
//! A faithful port of `bench-next/ldraw.py`, which stays the reference. Deliberate differences:
//! * reading is lock-free and parallel (the zip is memory-mapped and inflated per entry);
//! * flattened subfile meshes are cached library-wide, not per top-level part (same result);
//! * models keep every line byte-for-byte, so untouched lines survive a save unchanged;
//! * stud primitives are also recorded as instances (full matrix, primitive, colour) for the renderer;
//! * direct colours are written back as `0x2RRGGBB`, not as Python's internal negative code.

pub mod bricklink;
pub mod color;
pub mod library;
pub mod mesh;
pub mod model;
pub mod num;
pub mod runtime;
pub mod zipfs;

pub use color::{Color, ColorDef, Colors, Finish};
pub use library::{Error, Library, Lod, Result, canonical_name, norm};
pub use mesh::{Mesh, Stud};
pub use model::{Document, Eol, FileKind, FlatPart, Line, LineKind, ModelFile, PartLine};

/// One stud pitch in LDU.
pub const STUD: f64 = 20.0;
/// Plate height in LDU.
pub const PLATE: f64 = 8.0;
/// Brick height in LDU.
pub const BRICK: f64 = 24.0;

/// Male stud primitives (system scale) that another part's anti-stud can receive.
pub const MALE_STUDS: &[&str] = &[
    "stud.dat", "studa.dat", "stud2.dat", "stud2a.dat", "stud6.dat", "stud6a.dat",
    "stud9.dat", "stud10.dat", "stud13.dat", "stud15.dat", "stud17.dat", "stud17a.dat",
    "stud26.dat", "studel.dat", "studh.dat", "studp01.dat", "studx.dat",
    "stud-logo.dat", "stud-logo2.dat", "stud-logo3.dat", "stud-logo4.dat", "stud-logo5.dat",
    "stud2-logo.dat", "stud2-logo2.dat", "stud2-logo3.dat", "stud2-logo4.dat", "stud2-logo5.dat",
];

/// The hollow ones among [`MALE_STUDS`].
pub const HOLLOW_STUDS: &[&str] = &[
    "stud2.dat", "stud2a.dat", "stud6.dat", "stud6a.dat", "stud9.dat", "stud17.dat",
    "stud17a.dat", "stud2-logo.dat", "stud2-logo2.dat", "stud2-logo3.dat",
    "stud2-logo4.dat", "stud2-logo5.dat",
];

/// Product name shown to users.
pub const APP_NAME: &str = "SuperBrick";

/// Read a SuperBrick environment setting: `SB_<NAME>`, falling back to the pre-rename
/// `CAD_<NAME>` so existing setups keep working.
pub fn app_env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("SB_{name}")).or_else(|| std::env::var_os(format!("CAD_{name}")))
}

/// An installed data folder next to the binary: `<bin>/../share/superbrick/<sub>`, falling back
/// to the pre-rename `share/cad/<sub>`. Returns the first that exists, else the new path.
pub fn installed_share(sub: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?;
    // bin/../share (a prefix install), Contents/MacOS/../Resources/share (the macOS
    // app bundle), then the old cad install.
    let candidates = [
        bin.join("../share/superbrick").join(sub),
        bin.join("../Resources/share/superbrick").join(sub),
        bin.join("../share/cad").join(sub),
    ];
    let found = candidates.iter().find(|p| p.exists()).cloned();
    Some(found.unwrap_or_else(|| candidates[0].clone()))
}

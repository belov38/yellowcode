use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const SKIP: &[&str] = &[
    "ubidi_props_data.c",
    "ucase_props_data.c",
    "uchar_props_data.c",
    "uspoof.cpp",
];

struct Dirs {
    platform: Option<PathBuf>,
    common: PathBuf,
    i18n: PathBuf,
}

fn sources(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut files: Vec<PathBuf> = entries
        .map(|e| {
            e.unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
                .path()
        })
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .filter(|p| p.file_name().is_none_or(|n| !SKIP.iter().any(|s| n == *s)))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no *.{ext} in {}", dir.display());
    files
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Unix,
    MinGw,
    WindowsMsvc,
}

impl Target {
    fn platform_dir(self) -> Option<&'static str> {
        match self {
            Target::Unix => Some("include/unix"),
            Target::MinGw => Some("include/mingw"),
            Target::WindowsMsvc => None,
        }
    }
}

fn target() -> Target {
    let var = |k: &str| env::var(k).unwrap_or_default();
    let (os, target_env, abi, arch, endian) = (
        var("CARGO_CFG_TARGET_OS"),
        var("CARGO_CFG_TARGET_ENV"),
        var("CARGO_CFG_TARGET_ABI"),
        var("CARGO_CFG_TARGET_ARCH"),
        var("CARGO_CFG_TARGET_ENDIAN"),
    );
    assert!(
        endian == "little",
        "icu46-sys: big-endian targets are not supported (target endian {endian:?})"
    );
    match (os.as_str(), target_env.as_str()) {
        ("macos", _) => Target::Unix,
        ("linux", _) => {
            if !(target_env == "gnu" && matches!(arch.as_str(), "x86_64" | "aarch64")) {
                println!(
                    "cargo:warning=icu46-sys: target linux {arch} env={target_env:?} is \
                     unverified (verified: x86_64 and aarch64 glibc)"
                );
            }
            Target::Unix
        }
        ("windows", "gnu") if arch == "x86_64" && abi != "llvm" => Target::MinGw,
        ("windows", "msvc") => Target::WindowsMsvc,
        _ => panic!(
            "icu46-sys: unsupported target os={os:?} env={target_env:?} abi={abi:?} \
             arch={arch:?}; supported: macos, linux, x86_64 windows-gnu (GCC MinGW-w64), \
             windows-msvc (unverified)"
        ),
    }
}

fn build(cpp: bool, implementation: &str, dirs: &Dirs, target: Target) -> cc::Build {
    let mut b = cc::Build::new();
    b.cpp(cpp)
        .define("U_STATIC_IMPLEMENTATION", None)
        .define(implementation, None)
        .warnings(false)
        .extra_warnings(false)
        .cargo_warnings(false)
        .opt_level(2);
    if let Some(platform) = &dirs.platform {
        b.include(platform);
    }
    b.include(&dirs.common).include(&dirs.i18n);
    if cpp && target != Target::WindowsMsvc {
        b.std("gnu++98");
    }
    if cpp && target == Target::MinGw {
        b.cpp_link_stdlib_static(true);
    }
    b
}

fn main() {
    let target = target();
    let vendor = PathBuf::from("../../vendor/icu-4.6.1");
    let include = PathBuf::from("include");
    let dirs = Dirs {
        platform: target.platform_dir().map(PathBuf::from),
        common: vendor.join("common"),
        i18n: vendor.join("i18n"),
    };
    let stubdata = vendor.join("stubdata/stubdata.c");

    for p in [&vendor, &include] {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let mut objects = build(false, "U_I18N_IMPLEMENTATION", &dirs, target)
        .files(sources(&dirs.i18n, "c"))
        .compile_intermediates();
    objects.extend(
        build(true, "U_I18N_IMPLEMENTATION", &dirs, target)
            .files(sources(&dirs.i18n, "cpp"))
            .compile_intermediates(),
    );
    objects.extend(
        build(false, "U_COMMON_IMPLEMENTATION", &dirs, target)
            .files(sources(&dirs.common, "c"))
            .file(&stubdata)
            .compile_intermediates(),
    );
    build(true, "U_COMMON_IMPLEMENTATION", &dirs, target)
        .files(sources(&dirs.common, "cpp"))
        .objects(objects)
        .compile("icu46");

    if target != Target::Unix {
        println!("cargo:rustc-link-lib=advapi32");
    }
}

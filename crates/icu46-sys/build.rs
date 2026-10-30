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

fn mutex_test_sources(files: Vec<PathBuf>) -> Vec<PathBuf> {
    if env::var_os("CARGO_FEATURE_TESTING").is_none() {
        return files;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    files
        .into_iter()
        .map(|source| {
            let name = source.file_name().expect("source file name");
            let helper = match name.to_str() {
                Some("umutex.c") => "mutex_global.h",
                Some("uresbund.c") => "mutex_resource.h",
                Some("ucnv_bld.c") => "mutex_converter.h",
                Some("uinit.c") => "mutex_init.h",
                Some("icuplug.c") => "plugin_probe.h",
                Some("locid.cpp") => "locale_graph.h",
                Some("udata.cpp") => "common_graph.h",
                Some("normalizer2.cpp") => "normalizer_graph.h",
                Some("ucol_res.cpp") => "collator_graph.h",
                Some("ucol_bld.cpp") => "inverse_graph.h",
                Some("regexst.cpp") => "regex_graph.h",
                Some("uniset_props.cpp") => "property_graph.h",
                Some("rbbi.cpp") => "break_graph.h",
                Some("ustr_cnv.c") => "converter_graph.h",
                Some("cmemory.c") => "owned_memory.h",
                _ => return source,
            };
            let helper = PathBuf::from("tests/native").join(helper);
            let include = |path: &Path| {
                let path = std::path::absolute(path).expect("native test source");
                let path = path.to_string_lossy().replace('\\', "/");
                format!("#include \"{path}\"\n")
            };
            let wrapper = out.join(name);
            let body = if name == "umutex.c" {
                let mut body = fs::read_to_string(&source).expect("mutex source");
                for marker in ["umtx_lock(UMTX *mutex)\n{", "umtx_init(UMTX *mutex) {"] {
                    assert_eq!(body.matches(marker).count(), 1, "mutex observer anchor");
                    body = body.replace(
                        marker,
                        &(marker.to_owned() + "\n    mutex_test_observe(mutex);"),
                    );
                }
                "#include \"unicode/utypes.h\"\nstatic void mutex_test_observe(const void *p);\n"
                    .to_owned()
                    + &body
                    + &include(&helper)
            } else if name == "icuplug.c" {
                include(Path::new("tests/native/plugin_prefix.h"))
                    + &include(&source)
                    + &include(&helper)
            } else {
                include(&source) + &include(&helper)
            };
            fs::write(&wrapper, body).expect("native test wrapper");
            wrapper
        })
        .collect()
}

fn plugin_legacy_source(common: &Path) -> PathBuf {
    let source = std::path::absolute(common.join("icuplug.c")).expect("plugin source");
    let prefix = std::path::absolute("tests/native/plugin_prefix.h").expect("plugin prefix");
    let helper = std::path::absolute("tests/native/plugin_probe.h").expect("plugin helper");
    let symbols = fs::read_to_string(common.join("unicode/urename.h")).expect("renames");
    let mut body = "#include \"unicode/utypes.h\"\n".to_owned();
    for line in symbols.lines() {
        let mut words = line.split_whitespace();
        if words.next() == Some("#define")
            && let Some(name) = words.next().filter(|name| name.starts_with("uplug_"))
        {
            body += &format!("#undef {name}\n#define {name} legacy_{name}\n");
        }
    }
    body += "#undef U_ENABLE_DYLOAD\n#define U_ENABLE_DYLOAD 1\n#define MUTEX_PLUGIN_LEGACY 1\n";
    for path in [&prefix, &source, &helper] {
        body += &format!(
            "#include \"{}\"\n",
            path.to_string_lossy().replace('\\', "/")
        );
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("plugin_legacy.c");
    fs::write(&out, body).expect("legacy plugin wrapper");
    out
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
        .define("U_ENABLE_DYLOAD", "0")
        .define("U_ENABLE_TRACING", "0")
        .define("ICU_USE_THREADS", "1")
        .define("UCLN_NO_AUTO_CLEANUP", "1")
        .define(implementation, None)
        .warnings(false)
        .extra_warnings(false)
        .cargo_warnings(false)
        .opt_level(2);
    if env::var_os("CARGO_FEATURE_MANAGED").is_some() {
        b.define("U_MANAGED_MEMORY_ENABLED_46", "1");
    }
    if env::var_os("CARGO_FEATURE_TESTING").is_some() {
        b.define("U_MANAGED_MEMORY_TESTING_46", "1");
    }
    if let Some(platform) = &dirs.platform {
        b.include(platform);
    }
    b.include(&dirs.common).include(&dirs.i18n);
    let config = std::path::absolute("include/native_config.h").expect("native config pins");
    if target == Target::WindowsMsvc {
        b.flag(format!("/FI{}", config.display()));
    } else {
        b.flag("-include").flag(config);
    }
    if target != Target::Unix {
        b.define("_WIN32_WINNT", "0x0A00");
    }
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
    println!("cargo:rerun-if-changed=tests/native");

    let mut objects = build(false, "U_I18N_IMPLEMENTATION", &dirs, target)
        .files(sources(&dirs.i18n, "c"))
        .compile_intermediates();
    objects.extend(
        build(true, "U_I18N_IMPLEMENTATION", &dirs, target)
            .files(mutex_test_sources(sources(&dirs.i18n, "cpp")))
            .compile_intermediates(),
    );
    objects.extend(
        build(false, "U_COMMON_IMPLEMENTATION", &dirs, target)
            .files(mutex_test_sources(sources(&dirs.common, "c")))
            .file(&stubdata)
            .compile_intermediates(),
    );
    if env::var_os("CARGO_FEATURE_TESTING").is_some() {
        objects.extend(
            build(false, "U_COMMON_IMPLEMENTATION", &dirs, target)
                .file("tests/native/mutex_bridge.c")
                .file(plugin_legacy_source(&dirs.common))
                .compile_intermediates(),
        );
    }
    if env::var_os("CARGO_FEATURE_TESTING").is_some() {
        objects.extend(
            build(false, "U_COMMON_IMPLEMENTATION", &dirs, target)
                .file("tests/native/managed_abi.c")
                .compile_intermediates(),
        );
        objects.extend(
            build(true, "U_COMMON_IMPLEMENTATION", &dirs, target)
                .file("tests/native/managed_abi.cpp")
                .compile_intermediates(),
        );
    }
    build(true, "U_COMMON_IMPLEMENTATION", &dirs, target)
        .files(mutex_test_sources(sources(&dirs.common, "cpp")))
        .objects(objects)
        .compile("icu46");

    if target != Target::Unix {
        println!("cargo:rustc-link-lib=advapi32");
    }
}

//! FontTools adapter for explicit Unicode subsets, including every TTC face.
//!
//! Install the tested `fonttools==4.60.1` into the Python interpreter on `PATH`,
//! or select another interpreter with `REEL_FONT_PYTHON`. FontTools owns font
//! parsing, glyph/layout closure and serialization. Reel supplies the character
//! set, estimates working memory and atomically publishes verified output.
//! Tool selection and identity are cached for the current CLI process.
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

/// Return the actual Python and FontTools versions, locations and package-code
/// digest for resource-cache identity. `REEL_FONT_PYTHON` names an interpreter;
/// otherwise `python` (Windows) or `python3`/`python` (Unix) is resolved on PATH.
pub fn external_tools_identity() -> io::Result<String> {
    Ok(Tools::discover()?.identity.clone())
}

/// Subset a TTF, OpenType/CFF font or complete TTC using explicit codepoints.
///
/// Every original collection face remains in its original order. FontTools
/// retains all name records, font styles, applicable OpenType layout features,
/// glyph names and hinting. Each face's output Unicode cmap must equal the
/// intersection of its original coverage and `characters`; layout closure may
/// retain additional unencoded glyphs needed for shaping selected characters.
///
/// Existing output files are preserved. The returned workspace estimate is
/// twelve times the source length, 128 bytes per requested codepoint and 16 MiB
/// for interpreter/adapter work. The limit rejects oversized estimates before
/// loading the font; FontTools subprocess RSS is not a measured or hard-capped
/// Reel budget. The character set and tool identity belong in the caller's key.
pub fn prepare(
    source: &Path,
    output: &Path,
    characters: &BTreeSet<u32>,
    memory_bytes: u64,
) -> io::Result<u64> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("font output already exists: {}", output.display()),
        ));
    }
    if characters
        .iter()
        .any(|&value| char::from_u32(value).is_none())
    {
        return Err(invalid(
            "font character set contains an invalid Unicode scalar value",
        ));
    }
    let source_bytes = source.metadata()?.len();
    let workspace = source_bytes
        .checked_mul(12)
        .and_then(|bytes| bytes.checked_add((characters.len() as u64).checked_mul(128)?))
        .and_then(|bytes| bytes.checked_add(16 << 20))
        .ok_or_else(|| invalid("font workspace estimate overflows"))?;
    if workspace > memory_bytes {
        return Err(invalid(format!(
            "font preparation estimates {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }
    let tools = Tools::discover()?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".reel-font-")
        .tempfile_in(parent)?
        .into_temp_path();
    let mut unicodes = tempfile::NamedTempFile::new_in(parent)?;
    for value in characters {
        writeln!(unicodes, "{value:X}")?;
    }
    unicodes.flush()?;
    let mut command = tools.command("subset");
    command.arg(source).arg(&temporary).arg(unicodes.path());
    run(&mut command)?;
    OpenOptions::new()
        .write(true)
        .open(&temporary)?
        .sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| error.error)?;
    Ok(workspace)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

struct Tools {
    python: PathBuf,
    identity: String,
}

impl Tools {
    fn discover() -> io::Result<&'static Self> {
        static TOOLS: OnceLock<Tools> = OnceLock::new();
        if let Some(tools) = TOOLS.get() {
            return Ok(tools);
        }
        let python = if let Some(path) = std::env::var_os("REEL_FONT_PYTHON") {
            PathBuf::from(path).canonicalize().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("REEL_FONT_PYTHON must name an existing Python interpreter: {error}"),
                )
            })?
        } else {
            python_on_path()?
        };
        let mut tools = Self {
            python,
            identity: String::new(),
        };
        tools.identity = String::from_utf8(run(&mut tools.command("identity")).map_err(|error| {
            io::Error::new(error.kind(), format!(
                "font preparation requires fonttools==4.60.1 in the selected Python; install it or set REEL_FONT_PYTHON: {error}"
            ))
        })?)
            .map_err(|error| invalid(format!("FontTools identity is not UTF-8: {error}")))?;
        Ok(TOOLS.get_or_init(|| tools))
    }

    fn command(&self, operation: &str) -> Command {
        let mut command = Command::new(&self.python);
        command
            .args(["-X", "utf8", "-B", "-c", ADAPTER, operation])
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command
    }
}

fn python_on_path() -> io::Result<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["python.exe"]
    } else {
        &["python3", "python"]
    };
    if let Some(paths) = std::env::var_os("PATH") {
        let paths: Vec<_> = std::env::split_paths(&paths).collect();
        for name in names {
            for directory in &paths {
                let path = directory.join(name);
                if path.is_file() {
                    return path.canonicalize();
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "font preparation requires Python with fonttools==4.60.1; install it on PATH or set REEL_FONT_PYTHON",
    ))
}

fn run(command: &mut Command) -> io::Result<Vec<u8>> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let status = command
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .status()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("cannot start FontTools: {error}"))
        })?;
    if !status.success() {
        stderr.seek(SeekFrom::Start(0))?;
        let mut message = String::new();
        stderr.take(8192).read_to_string(&mut message)?;
        return Err(io::Error::other(format!(
            "FontTools failed ({status}): {message}"
        )));
    }
    if stdout.metadata()?.len() > 65536 {
        return Err(invalid("FontTools metadata exceeds 64 KiB"));
    }
    stdout.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes)?;
    Ok(bytes)
}

// FontTools loads each face, subsets the supplied codepoints and writes the
// original container format; the adapter validates cmap, names and styles.
const ADAPTER: &str = r#"
import hashlib
import json
from pathlib import Path
import platform
import sys
import fontTools
from fontTools import subset
from fontTools.ttLib import TTFont, TTCollection

def identity():
    package = Path(fontTools.__file__).resolve().parent
    digest = hashlib.sha256()
    for path in sorted(package.rglob('*')):
        if not path.is_file() or '__pycache__' in path.parts or path.suffix in ('.pyc', '.pyo'):
            continue
        digest.update(path.relative_to(package).as_posix().encode('utf-8'))
        digest.update(path.read_bytes())
    executable = Path(sys.executable).resolve()
    print(json.dumps(dict(python=str(executable), python_version=sys.version,
        implementation=platform.python_implementation(),
        python_sha256=hashlib.file_digest(executable.open('rb'), 'sha256').hexdigest(),
        fonttools_version=fontTools.__version__, package=str(package),
        package_sha256=digest.hexdigest()), ensure_ascii=True, sort_keys=True))

def load(path):
    with open(path, 'rb') as file:
        collection = file.read(4) == b'ttcf'
    owner = TTCollection(path, lazy=False, recalcTimestamp=False) if collection else TTFont(path, lazy=False, recalcTimestamp=False)
    return owner, owner.fonts if collection else [owner], collection

def coverage(font):
    return {codepoint for table in font['cmap'].tables if table.isUnicode()
        for codepoint in getattr(table, 'cmap', {})}

def names(font):
    return sorted((name.platformID, name.platEncID, name.langID, name.nameID, name.toBytes())
        for name in font['name'].names) if 'name' in font else []

def style(font):
    return (font.sfntVersion, font['head'].macStyle,
        tuple(getattr(font['OS/2'], key) for key in ('usWeightClass', 'usWidthClass', 'fsSelection'))
        if 'OS/2' in font else ())

def convert(source, output, unicode_file):
    characters = {int(line, 16) for line in Path(unicode_file).read_text(encoding='ascii').splitlines()}
    original, faces, collection = load(source)
    before = [(coverage(font) & characters, names(font), style(font)) for font in faces]
    try:
        for font in faces:
            options = subset.Options()
            options.name_IDs = ['*']
            options.name_languages = ['*']
            options.name_legacy = True
            options.layout_features = ['*']
            options.layout_scripts = ['*']
            options.hinting = True
            options.glyph_names = True
            options.notdef_glyph = True
            options.notdef_outline = True
            options.recalc_timestamp = False
            selector = subset.Subsetter(options=options)
            selector.populate(unicodes=characters)
            selector.subset(font)
        original.save(output)
    finally:
        original.close()
    prepared, faces, actual_collection = load(output)
    try:
        if actual_collection != collection or len(faces) != len(before):
            raise ValueError('font collection face count or format changed')
        for index, (font, (expected, original_names, original_style)) in enumerate(zip(faces, before)):
            if coverage(font) != expected:
                raise ValueError(f'font face {index} Unicode cmap changed outside the requested intersection')
            if names(font) != original_names or style(font) != original_style:
                raise ValueError(f'font face {index} naming or style changed')
        print(json.dumps(dict(faces=len(faces), codepoints=[len(expected) for expected, _, _ in before])))
    finally:
        prepared.close()

if sys.argv[1] == 'identity':
    identity()
elif sys.argv[1] == 'subset':
    convert(*sys.argv[2:])
else:
    raise ValueError('unknown FontTools adapter operation')
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_unicode_and_low_budget_preserve_output() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.ttf");
        let output = directory.path().join("output.ttf");
        std::fs::write(&source, b"source").unwrap();
        assert!(prepare(&source, &output, &BTreeSet::from([0xd800]), 128 << 20).is_err());
        assert!(prepare(&source, &output, &BTreeSet::from([65]), 1 << 20).is_err());
        assert!(!output.exists());
    }

    #[test]
    #[ignore = "requires FontTools in PATH Python or REEL_FONT_PYTHON"]
    fn fonttools_retains_collection_styles_and_only_requested_cmap() {
        let tools = Tools::discover().unwrap();
        assert!(
            external_tools_identity()
                .unwrap()
                .contains("fonttools_version")
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixtures = root.join("../reel-player/tests/fixtures/font-weight");
        let artifacts = root.join("../../target/reel-artifacts/font-tests");
        std::fs::create_dir_all(&artifacts).unwrap();
        let directory = tempfile::tempdir_in(artifacts).unwrap();
        let source = directory.path().join("collection.ttf");
        let mut command = Command::new(&tools.python);
        command.args(["-B", "-c", "from fontTools.ttLib import TTCollection,TTFont; import sys; c=TTCollection(); c.fonts=[TTFont(p) for p in sys.argv[2:]]; c.save(sys.argv[1])"])
            .arg(&source).arg(fixtures.join("400.ttf")).arg(fixtures.join("700.ttf"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        run(&mut command).unwrap();
        let output = directory.path().join("subset.ttf");
        prepare(&source, &output, &BTreeSet::from([65]), 128 << 20).unwrap();
        assert_eq!(&std::fs::read(&output).unwrap()[..4], b"ttcf");
        assert_eq!(
            prepare(&source, &output, &BTreeSet::from([65]), 128 << 20)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}

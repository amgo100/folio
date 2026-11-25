//! Reading a project from disk: finding its directory and loading its files
//! into a world.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use folio::{Diagnostic, PROJECT_FONT_DIR, World, WorldError};

/// The manifest's file name.
pub(crate) const MANIFEST: &str = "folio.toml";
/// The lockfile's file name.
pub(crate) const LOCKFILE: &str = "folio.lock";

/// The project directory: `root` if given, which must hold `folio.toml`,
/// or else the current directory or the nearest directory above it that
/// holds one.
pub(crate) fn find_root(root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = root {
        return if root.join(MANIFEST).is_file() {
            Ok(root.to_path_buf())
        } else {
            Err(format!("`{}` holds no {MANIFEST}", root.display()))
        };
    }
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    cwd.ancestors()
        .find(|dir| dir.join(MANIFEST).is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "no {MANIFEST} in `{}` or any directory above it",
                cwd.display()
            )
        })
}

/// Reads the lockfile, if the project has one.
pub(crate) fn read_lockfile(root: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(root.join(LOCKFILE)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A file of the project, by its path relative to the project directory.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProjectFile {
    /// The path, with `/` as separator.
    pub(crate) path: String,
    /// The file's contents.
    pub(crate) bytes: Vec<u8>,
}

/// The files of a project directory, in path order: every file except
/// `folio.toml` and `folio.lock`, hidden files and directories (whose names
/// start with `.`), the font directory (read by [`World::load_fonts`]) and
/// the directory `output` (where outputs are written). A symbolic link is
/// followed only to a file inside the project. Warns of files that cannot
/// be read.
pub(crate) fn read_files(
    root: &Path,
    output: Option<&str>,
    warnings: &mut Vec<Diagnostic>,
) -> io::Result<Vec<ProjectFile>> {
    let canonical = root.canonicalize()?;
    let mut files = Vec::new();
    let mut dirs = vec![String::new()];
    while let Some(dir) = dirs.pop() {
        let mut entries: Vec<_> = fs::read_dir(root.join(&dir))?.collect::<Result<_, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            // A name that is not Unicode cannot be named in a document.
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let file_type = entry.file_type()?;
            let is_file = if file_type.is_symlink() {
                // Not a directory, which could form a cycle, nor a file
                // outside the project, which reads are limited to.
                let target = entry.path().canonicalize();
                matches!(target, Ok(t) if t.starts_with(&canonical) && t.is_file())
            } else if file_type.is_dir() {
                if path != PROJECT_FONT_DIR && Some(path.as_str()) != output {
                    dirs.push(path);
                }
                continue;
            } else {
                file_type.is_file()
            };
            if !is_file || path == MANIFEST || path == LOCKFILE {
                continue;
            }
            match fs::read(entry.path()) {
                Ok(bytes) => files.push(ProjectFile { path, bytes }),
                Err(e) => warnings.push(unreadable(&path, &e.to_string())),
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The directory that outputs of a pattern are written into, before any
/// placeholder: `out` for `out/{stem}-{variant}.{ext}`; `None` when they
/// are written into the project directory itself.
pub(crate) fn output_dir(pattern: &str) -> Option<&str> {
    let fixed = &pattern[..pattern.find('{').unwrap_or(pattern.len())];
    fixed.rsplit_once('/').map(|(dir, _)| dir)
}

/// Sets the files of a project in a world: `.fol` and `.fsty` files as
/// sources, the others as data files. Warns of sources that are not UTF-8,
/// and leaves out files whose paths a document cannot name.
pub(crate) fn load(
    world: &mut World,
    files: Vec<ProjectFile>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(), WorldError> {
    for file in files {
        let source = matches!(
            Path::new(&file.path).extension().and_then(|e| e.to_str()),
            Some("fol" | "fsty")
        );
        let result = if source {
            match String::from_utf8(file.bytes) {
                Ok(text) => world.set_source(&file.path, &text).map(drop),
                Err(_) => {
                    warnings.push(unreadable(&file.path, "it is not UTF-8 text"));
                    continue;
                }
            }
        } else {
            world.set_file(&file.path, file.bytes)
        };
        match result {
            Ok(()) => {}
            Err(WorldError::InvalidPath(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The warning that a file cannot be read.
fn unreadable(path: &str, why: &str) -> Diagnostic {
    Diagnostic::warning(
        folio_syntax::DiagSpan::detached(),
        format!("cannot read `{path}`: {why}"),
    )
    .with_hint("a document that uses it reports it as not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_dirs() {
        assert_eq!(output_dir("out/{stem}-{variant}.{ext}"), Some("out"));
        assert_eq!(output_dir("build/pdf/{stem}.{ext}"), Some("build/pdf"));
        assert_eq!(output_dir("out/{variant}/{stem}.{ext}"), Some("out"));
        assert_eq!(output_dir("{stem}.{ext}"), None);
        assert_eq!(output_dir("out{variant}/{stem}.{ext}"), None);
    }

    #[test]
    fn reads_the_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: &[u8]| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        };
        write(MANIFEST, b"");
        write(LOCKFILE, b"");
        write("main.fol", b"= Forces");
        write("styles/exam.fsty", b"");
        write("data/points.csv", b"x,y\n");
        write(".git/config", b"");
        write("styles/.hidden.fsty", b"");
        write("fonts/serif.ttf", b"");
        write("out/main-exam.pdf", b"");
        write("outline/notes.fol", b"");

        let mut warnings = Vec::new();
        let files = read_files(root, Some("out"), &mut warnings).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "data/points.csv",
                "main.fol",
                "outline/notes.fol",
                "styles/exam.fsty"
            ]
        );
        assert!(warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_only_to_files_inside_the_project() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.csv"), b"x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        symlink(root.join("main.fol"), root.join("alias.fol")).unwrap();
        symlink(outside.path().join("secret.csv"), root.join("secret.csv")).unwrap();
        symlink(root, root.join("loop")).unwrap();

        let files = read_files(root, None, &mut Vec::new()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["alias.fol", "main.fol"]);
    }

    #[test]
    fn loads_sources_and_data() {
        let mut world = World::new(
            "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
        )
        .unwrap();
        let file = |path: &str, bytes: &[u8]| ProjectFile {
            path: path.into(),
            bytes: bytes.to_vec(),
        };
        let mut warnings = Vec::new();
        load(
            &mut world,
            vec![
                file("bad.fol", &[0xff, 0xfe]),
                file("data/points.csv", b"x,y\n"),
                file("main.fol", b"= Forces\n"),
                file("weird\\name.csv", b""),
            ],
            &mut warnings,
        )
        .unwrap();
        assert_eq!(world.source("main.fol").unwrap().text(), "= Forces\n");
        assert!(world.source("bad.fol").is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("`bad.fol`"));
    }
}

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
/// the files in `outputs` (which builds write). A symbolic link is
/// followed only to a file or directory inside the project, and not to a
/// directory it is in. Warns of files and directories that cannot be read.
pub(crate) fn read_files(
    root: &Path,
    outputs: &[&str],
    warnings: &mut Vec<Diagnostic>,
) -> io::Result<Vec<ProjectFile>> {
    let canonical = root.canonicalize()?;
    let mut files = Vec::new();
    // Each directory with the canonical paths of the directories it is in,
    // itself included, which a link must not lead back to.
    let mut dirs = vec![(String::new(), vec![canonical.clone()])];
    while let Some((dir, chain)) = dirs.pop() {
        let entries = fs::read_dir(root.join(&dir)).and_then(|entries| {
            entries
                .map(|entry| entry.and_then(|e| Ok((e.file_type()?, e))))
                .collect::<io::Result<Vec<_>>>()
        });
        let mut entries = match entries {
            Ok(entries) => entries,
            Err(e) if dir.is_empty() => return Err(e),
            Err(e) => {
                warnings.push(unreadable(&dir, &e.to_string()));
                continue;
            }
        };
        entries.sort_by_key(|(_, entry)| entry.file_name());
        for (file_type, entry) in entries {
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
            let target = if file_type.is_symlink() {
                // Reads are limited to the project.
                match entry.path().canonicalize() {
                    Ok(target) if target.starts_with(&canonical) => Some(target),
                    _ => continue,
                }
            } else {
                None
            };
            let is_dir = match &target {
                Some(target) => target.is_dir(),
                None => file_type.is_dir(),
            };
            if is_dir {
                if path == PROJECT_FONT_DIR {
                    continue;
                }
                let target = match target {
                    Some(target) => target,
                    None => match entry.path().canonicalize() {
                        Ok(target) => target,
                        Err(e) => {
                            warnings.push(unreadable(&path, &e.to_string()));
                            continue;
                        }
                    },
                };
                // A link to a directory it is in would never end.
                if chain.contains(&target) {
                    continue;
                }
                let mut chain = chain.clone();
                chain.push(target);
                dirs.push((path, chain));
                continue;
            }
            let is_file = match &target {
                Some(target) => target.is_file(),
                None => file_type.is_file(),
            };
            if !is_file || path == MANIFEST || path == LOCKFILE || outputs.contains(&path.as_str())
            {
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
                // Some editors start UTF-8 files with a byte-order mark,
                // which is not part of the text.
                Ok(text) => {
                    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
                    world.set_source(&file.path, text).map(drop)
                }
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
        write("out/figure.png", b"");
        write("outline/notes.fol", b"");

        let mut warnings = Vec::new();
        let files = read_files(root, &["out/main-exam.pdf"], &mut warnings).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "data/points.csv",
                "main.fol",
                "out/figure.png",
                "outline/notes.fol",
                "styles/exam.fsty"
            ]
        );
        assert!(warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn follows_links_only_inside_the_project() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.csv"), b"x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        fs::create_dir_all(root.join("shared/questions")).unwrap();
        fs::write(root.join("shared/questions/q1.fol"), b"").unwrap();
        symlink(root.join("main.fol"), root.join("alias.fol")).unwrap();
        symlink(outside.path().join("secret.csv"), root.join("secret.csv")).unwrap();
        symlink(outside.path(), root.join("elsewhere")).unwrap();
        symlink(root.join("shared/questions"), root.join("questions")).unwrap();
        symlink(root, root.join("loop")).unwrap();
        symlink(root.join("shared"), root.join("shared/questions/up")).unwrap();

        let files = read_files(root, &[], &mut Vec::new()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "alias.fol",
                "main.fol",
                "questions/q1.fol",
                "shared/questions/q1.fol",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn warns_of_unreadable_directories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("main.fol"), b"").unwrap();
        fs::create_dir(root.join("locked")).unwrap();
        fs::write(root.join("locked/x.csv"), b"").unwrap();
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        // Permissions do not stop a privileged user.
        let readable = fs::read_dir(root.join("locked")).is_ok();

        let mut warnings = Vec::new();
        let files = read_files(root, &[], &mut warnings);
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        let files = files.unwrap();
        if !readable {
            assert_eq!(files.len(), 1);
            assert!(warnings[0].message.contains("`locked`"));
        }
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
                file("bom.fol", "\u{FEFF}= Forces\n".as_bytes()),
                file("weird\\name.csv", b""),
            ],
            &mut warnings,
        )
        .unwrap();
        assert_eq!(world.source("main.fol").unwrap().text(), "= Forces\n");
        assert_eq!(world.source("bom.fol").unwrap().text(), "= Forces\n");
        assert!(world.source("bad.fol").is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("`bad.fol`"));
    }
}

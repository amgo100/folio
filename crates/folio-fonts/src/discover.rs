//! Finding and reading project and system fonts.
//!
//! These files can change while a compile runs, so they are read into
//! memory, never mapped (SPEC §2.3). Each file is read once per process
//! while its length and modification time stay the same, so compiles in
//! one process share it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use ecow::{EcoString, EcoVec};

use crate::book::FaceKey;
use crate::data::FontData;
use crate::file::{FontFile, FontOrigin};
use crate::font::{Font, FontStyle, FontVariant, push_family, stretch};

/// The project's font directory, relative to the directory that holds
/// `folio.toml`. Its fonts pin a build as the project's files do
/// (SPEC §2.4).
pub const PROJECT_FONT_DIR: &str = "fonts";

/// The extensions of font files, in lower case.
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Whether a path names a font file, by its extension.
pub fn is_font_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| FONT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The font files in a project's font directory ([`PROJECT_FONT_DIR`]) and
/// its subdirectories, in the order of their paths, which are relative to
/// `root` and use `/`. None when the project has no font directory.
pub fn project_fonts(root: &Path) -> io::Result<Vec<FontFile>> {
    let dir = root.join(PROJECT_FONT_DIR);
    let mut paths = Vec::new();
    match walk(&dir, &mut paths) {
        Err(e) if e.kind() == io::ErrorKind::NotFound && paths.is_empty() => {
            return Ok(Vec::new());
        }
        other => other?,
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            read(&path, FontOrigin::Project(relative.into()))
        })
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&path, out)?;
        } else if path.to_str().is_some_and(is_font_path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Reads a font file into memory, or takes it from the files this process
/// has read when it has not changed since.
fn read(path: &Path, origin: FontOrigin) -> io::Result<FontFile> {
    type Stamp = (u64, Option<SystemTime>);
    static READ: Mutex<Option<HashMap<PathBuf, (Stamp, FontFile)>>> = Mutex::new(None);

    let meta = fs::metadata(path)?;
    let stamp = (meta.len(), meta.modified().ok());
    {
        let read = READ.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((seen, file)) = read.as_ref().and_then(|read| read.get(path))
            && *seen == stamp
        {
            return Ok(if *file.origin() == origin {
                file.clone()
            } else {
                FontFile::new(origin, file.data().clone())
            });
        }
    }
    let file = FontFile::new(origin, FontData::from(fs::read(path)?));
    let mut read = READ.lock().unwrap_or_else(PoisonError::into_inner);
    read.get_or_insert_with(HashMap::new)
        .insert(path.to_owned(), (stamp, file.clone()));
    Ok(file)
}

/// A face installed on this machine, known by what discovery read of it.
/// Its file is read only when a document uses it ([`Self::load`]).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SystemFont {
    /// The file's path.
    pub path: EcoString,
    /// The index of the face in the file.
    pub index: u32,
    /// The PostScript name.
    pub postscript_name: EcoString,
    /// Its family names and variant.
    pub key: FaceKey,
}

impl SystemFont {
    /// Reads the face's file, once per process while it is unchanged.
    pub fn file(&self) -> io::Result<FontFile> {
        read(
            Path::new(self.path.as_str()),
            FontOrigin::System(self.path.clone()),
        )
    }

    /// Reads the face. Fails when the file cannot be read or no longer has
    /// the face.
    pub fn load(&self) -> io::Result<Font> {
        let file = self.file()?;
        let faces = file
            .faces()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.clone()))?;
        faces
            .iter()
            .find(|f| f.index() == self.index)
            .cloned()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} has no face {}", self.path, self.index),
                )
            })
    }
}

/// The faces installed on this machine, found with `fontdb` in the
/// platform's font directories, in the order of their paths. Discovery
/// reads each file's names and style but keeps none of them in memory.
pub fn system_fonts() -> Vec<SystemFont> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut out: Vec<SystemFont> = db
        .faces()
        .filter_map(|face| {
            let fontdb::Source::File(path) = &face.source else {
                return None;
            };
            let path = EcoString::from(path.to_str()?);
            let mut families = EcoVec::new();
            for (name, _) in &face.families {
                push_family(&mut families, name.as_str().into());
            }
            if families.is_empty() {
                return None;
            }
            let style = match face.style {
                fontdb::Style::Normal => FontStyle::Normal,
                fontdb::Style::Italic => FontStyle::Italic,
                fontdb::Style::Oblique => FontStyle::Oblique,
            };
            Some(SystemFont {
                path,
                index: face.index,
                postscript_name: face.post_script_name.as_str().into(),
                key: FaceKey {
                    families,
                    variant: FontVariant {
                        weight: face.weight.0,
                        style,
                        stretch: stretch(face.stretch.to_number()),
                    },
                },
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.path, a.index).cmp(&(&b.path, b.index)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled::bundled;

    #[test]
    fn reads_a_project_font_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(project_fonts(root.path()).unwrap().is_empty());

        let dir = root.path().join("fonts/serif");
        fs::create_dir_all(&dir).unwrap();
        let font = &bundled()[5];
        fs::write(dir.join("Serif.OTF"), font.data()).unwrap();
        fs::write(dir.join("notes.txt"), "not a font").unwrap();
        let found = project_fonts(root.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].origin(),
            &FontOrigin::Project("fonts/serif/Serif.OTF".into())
        );
        assert_eq!(found[0].hash(), font.hash());

        // Read once while unchanged.
        let again = project_fonts(root.path()).unwrap();
        assert!(std::ptr::eq(
            found[0].data().as_ptr(),
            again[0].data().as_ptr()
        ));
    }

    #[test]
    fn discovers_system_fonts_in_path_order() {
        // What is installed differs between machines; discovery must not
        // fail, and must list faces in a stable order.
        let fonts = system_fonts();
        assert!(
            fonts
                .windows(2)
                .all(|w| (&w[0].path, w[0].index) < (&w[1].path, w[1].index))
        );
        assert!(fonts.iter().all(|f| !f.key.families.is_empty()));
    }

    #[test]
    fn recognises_font_paths() {
        assert!(is_font_path("fonts/A.ttf"));
        assert!(is_font_path("A.OTC"));
        assert!(!is_font_path("fonts/A.woff2"));
        assert!(!is_font_path("fonts"));
    }
}

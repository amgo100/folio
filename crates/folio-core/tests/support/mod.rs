//! Evaluating a script in a database of its own, for the tests of the
//! evaluator that need no layout.

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::sync::Arc;

use ecow::EcoVec;
use folio_component::Registry;
use folio_core::db::{BuildMode, Db, Project, SourceFile};
use folio_core::diag::Reported;
use folio_core::eval::{Limits, evaluate as evaluate_project};
use folio_core::project::{Lockfile, Manifest, ProjectPath};
use folio_syntax::{FileId, Source};

/// A database for evaluation alone.
#[salsa::db]
#[derive(Clone, Default)]
struct Database {
    storage: salsa::Storage<Self>,
    text: Arc<folio_core::text::Cache>,
    layout: Arc<folio_core::layout::Cache>,
    pages: Arc<folio_core::layout::PageCache>,
    system_faces: Arc<folio_core::fonts::SystemFaces>,
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl Db for Database {
    fn text_cache(&self) -> &folio_core::text::Cache {
        &self.text
    }

    fn layout_cache(&self) -> &folio_core::layout::Cache {
        &self.layout
    }

    fn page_cache(&self) -> &folio_core::layout::PageCache {
        &self.pages
    }

    fn system_faces(&self) -> &folio_core::fonts::SystemFaces {
        &self.system_faces
    }
}

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
";

/// Evaluates `script` as a project's entry file within `limits`, and
/// returns the messages of the errors it reports.
pub(crate) fn evaluate(script: &str, limits: Limits) -> Vec<String> {
    let db = Database::default();
    let manifest = Manifest::parse(MANIFEST, FileId::from_raw(NonZeroU16::MIN)).unwrap();
    let path = ProjectPath::new("main.fol").unwrap();
    let id = FileId::from_raw(NonZeroU16::new(2).unwrap());
    let file = SourceFile::new(&db, path.clone(), Source::new(id, script.into()));
    let project = Project::new(
        &db,
        manifest,
        Lockfile::default(),
        BTreeMap::from([(path, file)]),
        BTreeMap::new(),
        EcoVec::new(),
        EcoVec::new(),
        EcoVec::new(),
        BuildMode::Build,
        Registry::default(),
        None,
        limits,
    );
    let _ = evaluate_project(&db, project, None);
    evaluate_project::accumulated::<Reported>(&db, project, None)
        .into_iter()
        .map(|reported| reported.0.message.to_string())
        .collect()
}

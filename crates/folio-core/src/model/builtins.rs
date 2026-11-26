//! The built-in elements (SPEC §4.2, §7.5, §7.6): their schemas, and which
//! of them content can call.

use std::sync::LazyLock;

use ecow::EcoVec;
use folio_component::{
    Body, ContentKind, CounterDecl, ElementKind, ElementSchema, FieldDecl, FieldType, Name, Value,
};

/// A built-in element.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Builtin {
    /// Its schema.
    pub schema: ElementSchema,
    /// Whether content can call it: `#figure(..)[..]`. The others are made
    /// by markup (`heading`, `strong`) or by the document model
    /// (`paragraph`, `section`, `list`).
    pub callable: bool,
}

/// The built-in elements, in a fixed order.
pub fn builtins() -> &'static [Builtin] {
    &BUILTINS
}

/// The built-in element with a name.
pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.schema.name.as_str() == name)
}

/// Whether an instance of a kind is a block in the flow, rather than
/// inline content in a paragraph. A note is anchored to its line, so it is
/// inline.
pub fn is_block(kind: ElementKind) -> bool {
    matches!(
        kind,
        ElementKind::Block | ElementKind::Float | ElementKind::Slide
    )
}

static BUILTINS: LazyLock<Vec<Builtin>> = LazyLock::new(|| {
    use ContentKind::{Blocks, Inline};
    use ElementKind::{Block, Float, Note};

    let content = |kind| Body::Content(kind);
    vec![
        // The document: its fields come from `#doc(..)`, its body is the
        // content of the entry file.
        element("doc", Block, true)
            .fields([
                optional("title", FieldType::Content),
                optional("subject", FieldType::Str),
            ])
            .body(content(Blocks))
            .build(),
        element("paragraph", Block, false)
            .body(content(Inline))
            .build(),
        element("heading", Block, true)
            .fields([FieldDecl::with_default(
                name("level"),
                FieldType::Int,
                Value::Int(1),
            )])
            .body(content(Inline))
            .counter()
            .ref_key("section")
            .build(),
        // A heading and the blocks after it, up to the next heading of the
        // same or a higher level.
        element("section", Block, false)
            .fields([FieldDecl::required(name("level"), FieldType::Int)])
            .body(content(Blocks))
            .build(),
        element("emph", ElementKind::Inline, true)
            .body(content(Inline))
            .build(),
        element("strong", ElementKind::Inline, true)
            .body(content(Inline))
            .build(),
        element("list", Block, false).body(content(Blocks)).build(),
        element("list-item", Block, false)
            .body(content(Blocks))
            .build(),
        element("enum", Block, false).body(content(Blocks)).build(),
        element("enum-item", Block, false)
            .fields([optional("number", FieldType::Int)])
            .body(content(Blocks))
            .build(),
        element("terms", Block, false).body(content(Blocks)).build(),
        element("terms-item", Block, false)
            .fields([FieldDecl::required(name("term"), FieldType::Content)])
            .body(content(Blocks))
            .build(),
        element("raw", ElementKind::Inline, true)
            .fields([
                FieldDecl::required(name("text"), FieldType::Str),
                FieldDecl::with_default(name("block"), FieldType::Bool, Value::Bool(false)),
                optional("lang", FieldType::Str),
            ])
            .positional(["text"])
            .build(),
        // An equation (SPEC §8.1). Evaluation adds its formula of the math
        // tree as the field `tree`, a value no field type describes, so the
        // schema leaves it out; until LaTeX input is read into the same
        // tree (#41), `tex` holds a LaTeX equation's source instead.
        element("math", ElementKind::Inline, false)
            .fields([
                FieldDecl::with_default(name("block"), FieldType::Bool, Value::Bool(false)),
                optional("tex", FieldType::Str),
            ])
            .build(),
        element("figure", Float, true)
            .fields([optional("caption", FieldType::Content)])
            .positional(["body", "caption"])
            .body(content(Blocks))
            .counter()
            .ref_key("figure")
            .build(),
        element("image", ElementKind::Inline, true)
            .fields([
                FieldDecl::required(name("path"), FieldType::Str),
                optional("alt", FieldType::Str),
            ])
            .positional(["path"])
            .build(),
        element("sidenote", Note, true)
            .body(content(Blocks))
            .counter()
            .build(),
        element("ref", ElementKind::Inline, false)
            .fields([
                FieldDecl::required(name("target"), FieldType::Ref),
                optional("supplement", FieldType::Content),
            ])
            .build(),
        element("link", ElementKind::Inline, false)
            .fields([FieldDecl::required(name("dest"), FieldType::Str)])
            .build(),
        // An error marker (SPEC §12.2): it stands where a construct failed
        // to parse or evaluate, so the rest of the document still compiles.
        // Its body is what it shows, and stylesheets style it as
        // `error { .. }`.
        element("error", ElementKind::Inline, false)
            .fields([FieldDecl::required(name("message"), FieldType::Str)])
            .body(content(Inline))
            .build(),
    ]
});

fn name(text: &str) -> Name {
    Name::new(text).expect("built-in names are identifiers")
}

fn optional(field: &str, ty: FieldType) -> FieldDecl {
    FieldDecl::with_default(name(field), ty, Value::None)
}

fn element(text: &str, kind: ElementKind, callable: bool) -> Builder {
    Builder(Builtin {
        schema: ElementSchema::new(name(text), kind),
        callable,
    })
}

struct Builder(Builtin);

impl Builder {
    fn fields(mut self, fields: impl IntoIterator<Item = FieldDecl>) -> Self {
        self.0.schema.fields = fields.into_iter().collect();
        self
    }

    fn positional<const N: usize>(mut self, fields: [&str; N]) -> Self {
        self.0.schema.positional = fields.into_iter().map(name).collect::<EcoVec<_>>();
        self
    }

    fn body(mut self, body: Body) -> Self {
        self.0.schema.body = body;
        self
    }

    fn counter(mut self) -> Self {
        self.0.schema.counter = Some(CounterDecl::default());
        self
    }

    fn ref_key(mut self, key: &str) -> Self {
        self.0.schema.ref_key = Some(name(key));
        self
    }

    fn build(self) -> Builtin {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use folio_component::{BODY, UNIVERSAL_FIELDS};

    /// The built-in schemas obey the rules the registry checks for
    /// components.
    #[test]
    fn schemas_are_well_formed() {
        let mut names = Vec::new();
        for builtin in builtins() {
            let schema = &builtin.schema;
            assert!(!names.contains(&schema.name), "{} twice", schema.name);
            names.push(schema.name.clone());
            for field in &schema.fields {
                let field_name = field.name.as_str();
                assert!(!UNIVERSAL_FIELDS.contains(&field_name), "{field_name}");
                assert_ne!(field_name, BODY);
                assert!(!field.ty.is_presentational());
                if let Some(default) = &field.default {
                    assert!(field.ty.accepts(default), "{field_name}");
                }
            }
            for positional in &schema.positional {
                assert!(
                    positional.as_str() == BODY || schema.field(positional.as_str()).is_some(),
                    "{positional}"
                );
            }
        }
        let figure = builtin("figure").unwrap();
        assert_eq!(
            figure.schema.positional_order().collect::<Vec<_>>(),
            ["body", "caption"]
        );
        assert!(builtin("paragraph").is_some_and(|b| !b.callable));
    }
}

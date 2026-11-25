//! Prototype for the PDF assumptions of SPEC §16.2 (issue #38).
//!
//! Writes test files with krilla 0.8.2, the version Typst 0.15 uses, and
//! reports what krilla accepts and what it writes:
//!
//! - `figure-*.pdf`: a PDF figure placed with `Surface::draw_pdf_page`, with
//!   and without validators, and converted to SVG first (§11.1);
//! - `math-*.pdf`: PDF 2.0 files whose formula is tagged with MathML
//!   structure elements, a MathML associated file, both, or alt text only,
//!   under PDF/A-4 and PDF/A-4f (§8.3, §11.1). krilla can write neither, so
//!   they are added afterwards with `lopdf`, as a krilla extension would;
//! - `transitions.pdf`: one slide per transition style, with step pages
//!   (§10.4). krilla has no `/Trans`, so it is added the same way.
//!
//! Usage: `cargo run --release -- <out-dir>`. Validate the output with
//! veraPDF as `tests/pdf/README.md` describes.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use krilla::Document;
use krilla::SerializeSettings;
use krilla::color::rgb;
use krilla::configure::{
    Accessibility, Archival, Configuration, ConfigurationBuilder, PdfVersion, Validator,
};
use krilla::destination::XyzDestination;
use krilla::geom::{Point, Rect, Size};
use krilla::metadata::{DateTime, Metadata};
use krilla::outline::{Outline, OutlineNode};
use krilla::page::{NumberingStyle, PageLabel, PageSettings};
use krilla::paint::Fill;
use krilla::pdf::{Pdf, PdfDocument};
use krilla::surface::Surface;
use krilla::tagging::{ContentTag, Identifier, Node, SpanTag, Tag, TagGroup, TagId, TagTree};
use krilla::text::{Font, TextDirection};
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};

const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: folio-pdf-prototype <out-dir>"),
    );
    std::fs::create_dir_all(&out).unwrap();
    let fonts = Fonts::load();

    println!("== Figures (§11.1)");
    figures(&out, &fonts);
    println!("\n== Math (§8.3)");
    math(&out, &fonts);
    println!("\n== Transitions (§10.4)");
    transitions(&out, &fonts);
}

/// Fonts from `crates/folio-fonts/bundled`.
struct Fonts {
    serif: Font,
    italic: Font,
    sans: Font,
}

impl Fonts {
    fn load() -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/folio-fonts/bundled");
        let load = |name: &str| {
            let data = std::fs::read(dir.join(name)).unwrap();
            Font::new(Arc::new(data).into(), 0).unwrap()
        };
        Fonts {
            serif: load("SourceSerif4-Regular.otf"),
            italic: load("SourceSerif4-It.otf"),
            sans: load("SourceSans3-Regular.otf"),
        }
    }
}

fn settings(configuration: Configuration) -> SerializeSettings {
    SerializeSettings {
        configuration,
        ..Default::default()
    }
}

fn config(version: PdfVersion, validators: &[Validator]) -> Configuration {
    let mut builder = ConfigurationBuilder::new().with_version(version);
    for v in validators {
        builder = builder.set_validator(*v);
    }
    builder.finish().unwrap()
}

/// Draws `text` as its own marked-content sequence and returns its identifier.
fn tagged_text(
    surface: &mut Surface,
    font: &Font,
    size: f32,
    at: (f32, f32),
    text: &str,
) -> Identifier {
    let id = surface.start_tagged(ContentTag::Span(SpanTag::empty()));
    surface.set_fill(Some(Fill {
        paint: rgb::Color::new(0, 0, 0).into(),
        ..Default::default()
    }));
    surface.draw_text(
        Point::from_xy(at.0, at.1),
        font.clone(),
        size,
        text,
        false,
        TextDirection::Auto,
    );
    surface.end_tagged();
    id
}

/// Metadata with a fixed date, so that the output is the same on every run.
fn metadata(title: &str) -> Metadata {
    Metadata::new()
        .title(title.into())
        .language("en".into())
        .creation_date(DateTime::new(2026).month(11).day(24))
}

/// A one-entry outline, which krilla's PDF/UA-1 validator requires.
fn outline(title: &str) -> Outline {
    let mut outline = Outline::new();
    outline.push_child(OutlineNode::new(
        title.into(),
        XyzDestination::new(0, Point::from_xy(0.0, 0.0)),
    ));
    outline
}

// ---------------------------------------------------------------------------
// Figures
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Placement {
    /// `Surface::draw_pdf_page`.
    Embed,
    /// The page converted to SVG with hayro-svg, then drawn with krilla-svg.
    Svg,
}

/// Name, figure, output version, validators, placement.
type FigureCase<'a> = (&'a str, &'a [u8], PdfVersion, &'a [Validator], Placement);

fn figures(out: &Path, fonts: &Fonts) {
    let fig_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../figure");
    let fig17 = std::fs::read(fig_dir.join("figure-1.7.pdf")).unwrap();
    let fig20 = std::fs::read(fig_dir.join("figure-2.0.pdf")).unwrap();

    let ua1 = Validator::Ua(Accessibility::UA1);
    let a2u = Validator::A(Archival::A2_U);
    let a2b = Validator::A(Archival::A2_B);
    let a4 = Validator::A(Archival::A4);
    let a4f = Validator::A(Archival::A4F);
    let cases: &[FigureCase] = &[
        (
            "figure-embed",
            &fig17,
            PdfVersion::Pdf17,
            &[],
            Placement::Embed,
        ),
        (
            "figure-embed-ua1",
            &fig17,
            PdfVersion::Pdf17,
            &[ua1],
            Placement::Embed,
        ),
        (
            "figure-embed-a2b",
            &fig17,
            PdfVersion::Pdf17,
            &[a2b],
            Placement::Embed,
        ),
        (
            "figure-embed-a4f",
            &fig20,
            PdfVersion::Pdf20,
            &[a4f],
            Placement::Embed,
        ),
        (
            "figure-embed-2.0-in-1.7",
            &fig20,
            PdfVersion::Pdf17,
            &[],
            Placement::Embed,
        ),
        (
            "figure-embed-2.0-in-2.0",
            &fig20,
            PdfVersion::Pdf20,
            &[],
            Placement::Embed,
        ),
        (
            "figure-svg-ua1-a2u",
            &fig17,
            PdfVersion::Pdf17,
            &[ua1, a2u],
            Placement::Svg,
        ),
        (
            "figure-svg-a4",
            &fig20,
            PdfVersion::Pdf20,
            &[a4],
            Placement::Svg,
        ),
    ];
    for &(name, fig, version, validators, placement) in cases {
        let result = figure_doc(fonts, fig, config(version, validators), placement);
        match result {
            Ok(bytes) => {
                std::fs::write(out.join(format!("{name}.pdf")), &bytes).unwrap();
                println!("{name}: written; {}", describe_xobjects(&bytes));
            }
            Err(e) => println!("{name}: refused by krilla: {e:?}"),
        }
    }
}

fn figure_doc(
    fonts: &Fonts,
    figure: &[u8],
    configuration: Configuration,
    placement: Placement,
) -> Result<Vec<u8>, String> {
    let mut document = Document::new_with(settings(configuration));
    let mut page = document.start_page_with(PageSettings::from_wh(320.0, 260.0).unwrap());
    let mut surface = page.surface();

    let caption = tagged_text(
        &mut surface,
        &fonts.serif,
        11.0,
        (40.0, 40.0),
        "Figure 1. Speed against time.",
    );

    surface.push_transform(&krilla::geom::Transform::from_translate(40.0, 60.0));
    let figure_id = surface.start_tagged(ContentTag::Other);
    let size = Size::from_wh(220.0, 150.0).unwrap();
    match placement {
        Placement::Embed => {
            let pdf = Pdf::new(figure.to_vec()).map_err(|e| format!("{e:?}"))?;
            surface.draw_pdf_page(&PdfDocument::new(Arc::new(pdf)), size, 0);
        }
        Placement::Svg => {
            use krilla_svg::{SurfaceExt, SvgSettings};
            let pdf =
                hayro_svg::hayro_syntax::Pdf::new(figure.to_vec()).map_err(|e| format!("{e:?}"))?;
            let cache = hayro_svg::RenderCache::new();
            let svg = hayro_svg::convert(
                &pdf.pages()[0],
                &cache,
                &hayro_svg::hayro_interpret::InterpreterSettings::default(),
                &hayro_svg::SvgRenderSettings::default(),
            );
            let tree =
                usvg::Tree::from_str(&svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
            surface.draw_svg(&tree, size, SvgSettings::default());
        }
    }
    surface.end_tagged();
    surface.pop();
    surface.finish();
    page.finish();

    let mut tree = TagTree::new().with_lang(Some("en".into()));
    let mut figure = TagGroup::new(Tag::Figure(Some(
        "Graph of speed against time: a curve rising from zero to a maximum, then falling.".into(),
    )));
    figure.push(Node::Leaf(figure_id));
    tree.push(figure);
    tree.push(TagGroup::with_children(
        Tag::Caption,
        vec![Node::Group(TagGroup::with_children(
            Tag::P,
            vec![Node::Leaf(caption)],
        ))],
    ));
    document.set_tag_tree(tree);
    document.set_metadata(metadata("Figure test"));
    document.set_outline(outline("Figure test"));
    document.finish().map_err(|e| format!("{e:?}"))
}

/// Summarises the XObjects of a PDF: how many forms and images, and whether
/// the forms keep fonts and text operators.
fn describe_xobjects(bytes: &[u8]) -> String {
    let doc = lopdf::Document::load_mem(bytes).unwrap();
    let mut forms = 0;
    let mut forms_with_text = 0;
    let mut forms_with_fonts = 0;
    let mut images = 0;
    let mut fonts = 0;
    for obj in doc.objects.values() {
        let Ok(stream) = obj.as_stream() else {
            if let Ok(d) = obj.as_dict()
                && d.get(b"Type").and_then(|t| t.as_name()).ok() == Some(b"Font".as_slice())
            {
                fonts += 1;
            }
            continue;
        };
        match stream.dict.get(b"Subtype").and_then(|s| s.as_name()).ok() {
            Some(b"Form") => {
                forms += 1;
                let content = stream
                    .decompressed_content()
                    .unwrap_or_else(|_| stream.content.clone());
                let text = String::from_utf8_lossy(&content);
                if text.contains("BT") && (text.contains("Tj") || text.contains("TJ")) {
                    forms_with_text += 1;
                }
                if let Ok(res) = stream.dict.get(b"Resources") {
                    let res = match res {
                        Object::Reference(r) => doc.get_dictionary(*r).ok(),
                        Object::Dictionary(d) => Some(d),
                        _ => None,
                    };
                    if res.is_some_and(|r| r.has(b"Font")) {
                        forms_with_fonts += 1;
                    }
                }
            }
            Some(b"Image") => images += 1,
            _ => {}
        }
    }
    format!(
        "{forms} form XObjects ({forms_with_text} with text operators, {forms_with_fonts} with a Font resource), {images} image XObjects, {fonts} font dictionaries"
    )
}

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum MathTagging {
    /// `Formula` with alt text only.
    Alt,
    /// MathML structure elements under `Formula`.
    StructureElements,
    /// A MathML associated file on `Formula`.
    AssociatedFile,
    /// Both.
    Both,
}

const FORMULA_MATHML: &str = "<math xmlns=\"http://www.w3.org/1998/Math/MathML\"><mrow><msup><mi>x</mi><mn>2</mn></msup><mo>+</mo><mn>1</mn></mrow></math>";

fn math(out: &Path, fonts: &Fonts) {
    let cases = [
        ("math-alt-a4", Archival::A4, MathTagging::Alt),
        ("math-se-a4", Archival::A4, MathTagging::StructureElements),
        ("math-af-a4", Archival::A4, MathTagging::AssociatedFile),
        ("math-af-a4f", Archival::A4F, MathTagging::AssociatedFile),
        ("math-both-a4f", Archival::A4F, MathTagging::Both),
    ];
    for (name, archival, tagging) in cases {
        match math_doc(fonts, archival, tagging) {
            Ok(bytes) => {
                std::fs::write(out.join(format!("{name}.pdf")), &bytes).unwrap();
                println!("{name}: written");
            }
            Err(e) => println!("{name}: refused: {e}"),
        }
    }
}

fn math_doc(fonts: &Fonts, archival: Archival, tagging: MathTagging) -> Result<Vec<u8>, String> {
    let configuration = config(PdfVersion::Pdf20, &[Validator::A(archival)]);
    let mut document = Document::new_with(settings(configuration));
    let mut page = document.start_page_with(PageSettings::from_wh(320.0, 120.0).unwrap());
    let mut surface = page.surface();

    let before = tagged_text(
        &mut surface,
        &fonts.serif,
        12.0,
        (40.0, 60.0),
        "The value is ",
    );
    let x = tagged_text(&mut surface, &fonts.italic, 12.0, (113.0, 60.0), "x");
    let two = tagged_text(&mut surface, &fonts.serif, 8.0, (119.5, 54.0), "2");
    let plus = tagged_text(&mut surface, &fonts.serif, 12.0, (127.0, 60.0), "+");
    let one = tagged_text(&mut surface, &fonts.serif, 12.0, (137.0, 60.0), "1");
    let after = tagged_text(&mut surface, &fonts.serif, 12.0, (143.5, 60.0), ".");
    surface.finish();
    page.finish();

    // MathML elements are written as spans with an ID that names them; the
    // post-processing below turns them into elements of the MathML namespace.
    let counter = std::cell::Cell::new(0);
    let mml = |name: &str, children: Vec<Node>| {
        counter.set(counter.get() + 1);
        let id = TagId::from(format!("mml:{name}#{}", counter.get()).into_bytes());
        Node::Group(TagGroup::with_children(
            Tag::Span.with_id(Some(id)),
            children,
        ))
    };
    let formula_children = if matches!(tagging, MathTagging::StructureElements | MathTagging::Both)
    {
        let msup = mml(
            "msup",
            vec![
                mml("mi", vec![Node::Leaf(x)]),
                mml("mn", vec![Node::Leaf(two)]),
            ],
        );
        let mo = mml("mo", vec![Node::Leaf(plus)]);
        let mn = mml("mn", vec![Node::Leaf(one)]);
        let mrow = mml("mrow", vec![msup, mo, mn]);
        vec![mml("math", vec![mrow])]
    } else {
        vec![
            Node::Leaf(x),
            Node::Leaf(two),
            Node::Leaf(plus),
            Node::Leaf(one),
        ]
    };
    let formula = TagGroup::with_children(
        Tag::Formula(Some("x squared plus 1".into())),
        formula_children,
    );
    let p = TagGroup::with_children(
        Tag::P,
        vec![Node::Leaf(before), Node::Group(formula), Node::Leaf(after)],
    );
    let mut tree = TagTree::new().with_lang(Some("en".into()));
    tree.push(p);
    document.set_tag_tree(tree);
    // No outline: krilla 0.8.2 writes outline destinations that are not
    // structure destinations, which PDF/UA-2 (8.8) requires.
    document.set_metadata(metadata("Math test"));
    let bytes = document.finish().map_err(|e| format!("{e:?}"))?;

    postprocess_math(&bytes, tagging)
}

/// Adds what krilla cannot write: MathML structure elements, a MathML
/// associated file, and the PDF/UA-2 identification.
fn postprocess_math(bytes: &[u8], tagging: MathTagging) -> Result<Vec<u8>, String> {
    let mut doc = lopdf::Document::load_mem(bytes).map_err(|e| e.to_string())?;

    fix_pdf2_namespace(&mut doc);

    let ns = doc.add_object(Dictionary::from_iter([
        ("Type", Object::Name(b"Namespace".to_vec())),
        (
            "NS",
            Object::String(MATHML_NS.into(), StringFormat::Literal),
        ),
    ]));

    // Struct elements by ID; the formula.
    let mut mathml_elems: Vec<(ObjectId, String)> = Vec::new();
    let mut formula: Option<ObjectId> = None;
    for (&id, obj) in &doc.objects {
        let Ok(d) = obj.as_dict() else { continue };
        if d.get(b"Type").and_then(|t| t.as_name()).ok() != Some(b"StructElem".as_slice()) {
            continue;
        }
        if let Ok(Object::String(s, _)) = d.get(b"ID")
            && let Some(rest) = s.strip_prefix(b"Umml:")
        {
            let name = String::from_utf8_lossy(rest)
                .split('#')
                .next()
                .unwrap()
                .to_owned();
            mathml_elems.push((id, name));
        }
        if d.get(b"S").and_then(|s| s.as_name()).ok() == Some(b"Formula".as_slice()) {
            formula = Some(id);
        }
    }
    let formula = formula.ok_or("no Formula element")?;

    for (id, name) in &mathml_elems {
        let d = doc.get_dictionary_mut(*id).unwrap();
        d.set("S", Object::Name(name.clone().into_bytes()));
        d.set("NS", Object::Reference(ns));
        d.remove(b"ID");
    }

    // Struct tree root: register the namespace, drop the IDs of the MathML
    // elements from the ID tree.
    let root_id = doc
        .catalog()
        .unwrap()
        .get(b"StructTreeRoot")
        .unwrap()
        .as_reference()
        .unwrap();
    let id_tree = doc
        .get_dictionary(root_id)
        .unwrap()
        .get(b"IDTree")
        .ok()
        .cloned();
    {
        let root = doc.get_dictionary_mut(root_id).unwrap();
        let mut namespaces = root.get(b"Namespaces").ok().cloned();
        match &mut namespaces {
            Some(Object::Array(a)) => a.push(Object::Reference(ns)),
            Some(Object::Reference(_)) => {
                return Err("indirect Namespaces array not handled".into());
            }
            _ => namespaces = Some(Object::Array(vec![Object::Reference(ns)])),
        }
        root.set("Namespaces", namespaces.unwrap());
    }
    if let Some(id_tree) = id_tree {
        prune_id_tree(&mut doc, &id_tree);
        let empty = match &id_tree {
            Object::Reference(r) => doc.get_dictionary(*r).ok().cloned(),
            Object::Dictionary(d) => Some(d.clone()),
            _ => None,
        }
        .is_some_and(|d| {
            d.get(b"Names")
                .and_then(|n| n.as_array())
                .is_ok_and(|a| a.is_empty())
                && !d.has(b"Kids")
        });
        if empty {
            doc.get_dictionary_mut(root_id).unwrap().remove(b"IDTree");
        }
    }

    if matches!(tagging, MathTagging::AssociatedFile | MathTagging::Both) {
        let mut stream = Stream::new(
            Dictionary::from_iter([
                ("Type", Object::Name(b"EmbeddedFile".to_vec())),
                ("Subtype", Object::Name(b"application/mathml+xml".to_vec())),
                (
                    "Params",
                    Object::Dictionary(Dictionary::from_iter([(
                        "ModDate",
                        Object::String(b"D:20261124000000Z".to_vec(), StringFormat::Literal),
                    )])),
                ),
            ]),
            FORMULA_MATHML.as_bytes().to_vec(),
        );
        stream.dict.set("Length", FORMULA_MATHML.len() as i64);
        let ef = doc.add_object(stream);
        let name = Object::String(b"formula-1.mml".to_vec(), StringFormat::Literal);
        let filespec = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Filespec".to_vec())),
            ("F", name.clone()),
            ("UF", name),
            (
                "EF",
                Object::Dictionary(Dictionary::from_iter([
                    ("F", Object::Reference(ef)),
                    ("UF", Object::Reference(ef)),
                ])),
            ),
            ("AFRelationship", Object::Name(b"Supplement".to_vec())),
            (
                "Desc",
                Object::String(b"MathML representation".to_vec(), StringFormat::Literal),
            ),
        ]));
        doc.get_dictionary_mut(formula)
            .unwrap()
            .set("AF", Object::Array(vec![Object::Reference(filespec)]));
    }

    identify_ua2(&mut doc)?;

    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Corrects what krilla 0.8.2 writes and its unreleased `main` already fixes:
/// the PDF 2.0 namespace URI (pdf-writer 0.15.0 writes
/// `https://www.iso.org/pdf2/ssn`; ISO 32000-2 §14.8.6.1 has
/// `http://iso.org/pdf2/ssn`), and the root `Document` element, which krilla
/// PR #450 moves into the PDF 2.0 namespace.
fn fix_pdf2_namespace(doc: &mut lopdf::Document) {
    let mut pdf2 = None;
    for (&id, obj) in doc.objects.iter_mut() {
        let Ok(d) = obj.as_dict_mut() else { continue };
        if let Ok(Object::String(uri, _)) = d.get(b"NS")
            && uri.as_slice() == b"https://www.iso.org/pdf2/ssn"
        {
            d.set(
                "NS",
                Object::String(b"http://iso.org/pdf2/ssn".to_vec(), StringFormat::Literal),
            );
            pdf2 = Some(id);
        }
    }
    let Some(pdf2) = pdf2 else { return };
    for obj in doc.objects.values_mut() {
        let Ok(d) = obj.as_dict_mut() else { continue };
        if d.get(b"Type").and_then(|t| t.as_name()).ok() == Some(b"StructElem".as_slice())
            && d.get(b"S").and_then(|s| s.as_name()).ok() == Some(b"Document".as_slice())
        {
            d.set("NS", Object::Reference(pdf2));
        }
    }
}

fn prune_id_tree(doc: &mut lopdf::Document, node: &Object) {
    let id = match node {
        Object::Reference(r) => *r,
        _ => return,
    };
    let Ok(d) = doc.get_dictionary(id).cloned() else {
        return;
    };
    if let Ok(kids) = d.get(b"Kids").and_then(|k| k.as_array()) {
        for kid in kids.clone() {
            prune_id_tree(doc, &kid);
        }
    }
    if let Ok(names) = d.get(b"Names").and_then(|n| n.as_array()) {
        let mut kept = Vec::new();
        for pair in names.chunks(2) {
            if let Object::String(s, _) = &pair[0]
                && s.starts_with(b"Umml:")
            {
                continue;
            }
            kept.extend_from_slice(pair);
        }
        let d = doc.get_dictionary_mut(id).unwrap();
        if let Some((Object::String(first, _), Object::String(last, _))) =
            kept.first().zip(kept.get(kept.len().saturating_sub(2)))
        {
            let limits = Object::Array(vec![
                Object::String(first.clone(), StringFormat::Literal),
                Object::String(last.clone(), StringFormat::Literal),
            ]);
            if d.has(b"Limits") {
                d.set("Limits", limits);
            }
        }
        d.set("Names", Object::Array(kept));
    }
}

/// Declares PDF/UA-2 and WTPDF 1.0 accessibility in the XMP metadata and
/// sets `DisplayDocTitle`, which krilla writes only under its own UA-1
/// validator.
fn identify_ua2(doc: &mut lopdf::Document) -> Result<(), String> {
    let catalog_id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    let meta_id = doc
        .get_dictionary(catalog_id)
        .unwrap()
        .get(b"Metadata")
        .unwrap()
        .as_reference()
        .unwrap();
    let stream = doc
        .get_object_mut(meta_id)
        .unwrap()
        .as_stream_mut()
        .unwrap();
    let xmp = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    let xmp = String::from_utf8(xmp).map_err(|e| e.to_string())?;
    let ua = concat!(
        "<rdf:Description rdf:about=\"\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\">",
        "<pdfuaid:part>2</pdfuaid:part><pdfuaid:rev>2024</pdfuaid:rev></rdf:Description>",
        "<rdf:Description rdf:about=\"\" xmlns:pdfd=\"http://pdfa.org/declarations/\">",
        "<pdfd:declarations><rdf:Bag><rdf:li rdf:parseType=\"Resource\">",
        "<pdfd:conformsTo>http://pdfa.org/declarations/wtpdf#accessibility1.0</pdfd:conformsTo>",
        "</rdf:li></rdf:Bag></pdfd:declarations></rdf:Description>",
    );
    if !xmp.contains("</rdf:RDF>") {
        return Err(
            "XMP metadata has no </rdf:RDF> to insert the PDF/UA-2 identification before".into(),
        );
    }
    let xmp = xmp.replacen("</rdf:RDF>", &format!("{ua}</rdf:RDF>"), 1);
    stream.dict.remove(b"Filter");
    stream.set_content(xmp.into_bytes());

    let catalog = doc.get_dictionary_mut(catalog_id).unwrap();
    let mut prefs = match catalog.get(b"ViewerPreferences") {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    prefs.set("DisplayDocTitle", true);
    catalog.set("ViewerPreferences", Object::Dictionary(prefs));
    Ok(())
}

// ---------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------

/// The transition styles of ISO 32000 §12.4.4.1, with the entries that pick
/// a visible variant.
const TRANSITIONS: &[(&str, &[(&str, &str)])] = &[
    ("Split", &[("Dm", "/H"), ("M", "/O")]),
    ("Blinds", &[("Dm", "/H")]),
    ("Box", &[("M", "/O")]),
    ("Wipe", &[("Di", "0")]),
    ("Dissolve", &[]),
    ("Glitter", &[("Di", "0")]),
    ("Fly", &[("Di", "0")]),
    ("Push", &[("Di", "0")]),
    ("Cover", &[("Di", "0")]),
    ("Uncover", &[("Di", "0")]),
    ("Fade", &[]),
];

/// Seconds per transition: long enough to see, and to catch in a screenshot.
const TRANSITION_SECONDS: f32 = 3.0;

fn transitions(out: &Path, fonts: &Fonts) {
    let mut document = Document::new_with(settings(config(PdfVersion::Pdf17, &[])));
    // (page index, style) of each slide's first page.
    let mut firsts: BTreeMap<usize, &str> = BTreeMap::new();
    let mut index = 0;
    for (slide, (style, _)) in TRANSITIONS.iter().enumerate() {
        let number = slide + 1;
        // Light and dark slides alternate so that every transition is visible.
        let (bg, fg) = if slide % 2 == 0 {
            ((250, 250, 240), (20, 20, 60))
        } else {
            ((30, 40, 90), (250, 250, 240))
        };
        for step in 0..2 {
            // Page labels as §11.1: 1a, 1b for the steps of slide 1. krilla
            // starts a label range on every page that has one, so each step
            // page carries its own letter as the start of its range.
            let label = PageLabel::new(
                Some(NumberingStyle::LowerAlpha),
                Some(number.to_string()),
                NonZeroU32::new(step + 1),
            );
            let mut page = document.start_page_with(
                PageSettings::from_wh(720.0, 540.0)
                    .unwrap()
                    .with_page_label(label),
            );
            let mut surface = page.surface();
            let path = krilla::geom::PathBuilder::new();
            let mut path = path;
            path.push_rect(Rect::from_xywh(0.0, 0.0, 720.0, 540.0).unwrap());
            surface.set_fill(Some(Fill {
                paint: rgb::Color::new(bg.0, bg.1, bg.2).into(),
                ..Default::default()
            }));
            surface.draw_path(&path.finish().unwrap());
            surface.set_fill(Some(Fill {
                paint: rgb::Color::new(fg.0, fg.1, fg.2).into(),
                ..Default::default()
            }));
            let title = format!("{number}. {style}");
            surface.draw_text(
                Point::from_xy(60.0, 140.0),
                fonts.sans.clone(),
                64.0,
                &title,
                false,
                TextDirection::Auto,
            );
            let line = if step == 0 {
                format!("Step 1 of 2: this page has /Trans /S /{style}")
            } else {
                "Step 2 of 2: no /Trans, so this step should cut".to_owned()
            };
            surface.draw_text(
                Point::from_xy(60.0, 220.0),
                fonts.sans.clone(),
                24.0,
                &line,
                false,
                TextDirection::Auto,
            );
            if step == 1 {
                surface.draw_text(
                    Point::from_xy(60.0, 300.0),
                    fonts.serif.clone(),
                    40.0,
                    "A step appears.",
                    false,
                    TextDirection::Auto,
                );
            }
            surface.finish();
            page.finish();
            if step == 0 {
                firsts.insert(index, style);
            }
            index += 1;
        }
    }
    document.set_metadata(metadata("Transition test"));
    let bytes = document.finish().unwrap();

    let mut doc = lopdf::Document::load_mem(&bytes).unwrap();
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for (&i, &style) in &firsts {
        let extra = TRANSITIONS.iter().find(|(s, _)| *s == style).unwrap().1;
        let mut trans = Dictionary::from_iter([
            ("Type", Object::Name(b"Trans".to_vec())),
            ("S", Object::Name(style.as_bytes().to_vec())),
            ("D", Object::Real(TRANSITION_SECONDS)),
        ]);
        for (k, v) in extra {
            let value = match v.strip_prefix('/') {
                Some(n) => Object::Name(n.as_bytes().to_vec()),
                None => Object::Integer(v.parse().unwrap()),
            };
            trans.set(*k, value);
        }
        doc.get_dictionary_mut(pages[i])
            .unwrap()
            .set("Trans", Object::Dictionary(trans));
    }
    let mut out_bytes = Vec::new();
    doc.save_to(&mut out_bytes).unwrap();
    std::fs::write(out.join("transitions.pdf"), out_bytes).unwrap();
    println!(
        "transitions: written, {} pages, {} slides",
        pages.len(),
        firsts.len()
    );
}

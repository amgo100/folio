//! The property registry (SPEC §3.2, §5.2, §10.2): every stylesheet
//! property a build knows, with its type, initial value, inheritance and
//! whether it affects layout or only paint, sorted into property groups.
//!
//! The built-in properties come in the groups of SPEC §5.2. Each component
//! in use adds one group for its properties, named after its namespace. The
//! `docx-*` and `pptx-*` properties of the export group are open-ended: the
//! registry of a build holds those its stylesheets name.

use std::fmt::{self, Debug, Formatter};
use std::sync::LazyLock;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{
    Abs, Color, Effect, Em, Name, PropertyDecl, PropertyType, QualifiedName, Value,
};

use crate::component::ComponentHost;

/// A property group: the unit computed styles are shared in (SPEC §5.2).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GroupId(pub(crate) u16);

impl GroupId {
    /// `font`, `font-size`, `color`, ...
    pub const TEXT: Self = Self(0);
    /// `leading`, `indent`, `align`, ...
    pub const PARAGRAPH: Self = Self(1);
    /// `space-before`, `padding`, `border`, ...
    pub const BLOCK: Self = Self(2);
    /// `numbering`, `marker`, `ref-format`, ...
    pub const NUMBERING: Self = Self(3);
    /// `display`, `show-only`.
    pub const VISIBILITY: Self = Self(4);
    /// `template`.
    pub const TEMPLATES: Self = Self(5);
    /// `placement`, `side`, `wrap`, ...
    pub const PLACEMENT: Self = Self(6);
    /// `page-size`, `margin-*`, `header`, ...
    pub const PAGE: Self = Self(7);
    /// `stroke`, `fill`, `palette`, ...
    pub const GRAPHICS: Self = Self(8);
    /// `entrance`, `exit`, `transition`, ...
    pub const ANIMATION: Self = Self(9);
    /// `reveal`, `slide-break`, `overflow`, ...
    pub const SLIDES: Self = Self(10);
    /// `export-name`, `export-as`, `docx-*`, `pptx-*`.
    pub const EXPORT: Self = Self(11);
    /// `math-font`, `math-style`, `decimal-marker`.
    pub const MATH: Self = Self(12);

    /// The number of built-in groups.
    const BUILTIN: u16 = 13;

    /// The index of the group in a computed style.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A property of the registry.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PropId(pub(crate) u32);

impl PropId {
    /// The index of the property in the registry.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A property as the registry knows it.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Property {
    /// Its name: unqualified for a built-in property, qualified with its
    /// component's namespace for a component's.
    pub name: QualifiedName,
    /// The type of its values.
    pub ty: PropertyType,
    /// Its value where no rule sets it and it does not inherit one.
    pub initial: Value,
    /// Whether an element without a value takes its parent's (SPEC §5.1).
    pub inherited: bool,
    /// Whether a change moves anything or only repaints (SPEC §10.2).
    pub effect: Effect,
    /// The group it is computed and shared in.
    pub group: GroupId,
    /// Its position within the group.
    pub slot: u16,
}

/// The properties a build knows, and their groups.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Properties {
    props: Vec<Property>,
    /// The name of each group, by index: `text`, ..., or a namespace.
    groups: Vec<EcoString>,
    /// The properties of each group, in slot order.
    members: Vec<Vec<PropId>>,
    /// Built-in and office properties by unqualified name, component
    /// properties by qualified name, sorted.
    by_name: Vec<(QualifiedName, PropId)>,
}

impl Debug for Properties {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Properties")
            .field("len", &self.props.len())
            .field("groups", &self.groups)
            .finish()
    }
}

impl Properties {
    /// The built-in properties, the properties of the components in use, and
    /// the `docx-*` and `pptx-*` properties in `office`.
    pub fn new(host: Option<&ComponentHost>, office: impl IntoIterator<Item = Name>) -> Self {
        let mut this = Self {
            props: Vec::new(),
            groups: GROUP_NAMES.iter().map(|&n| n.into()).collect(),
            members: vec![Vec::new(); GroupId::BUILTIN as usize],
            by_name: Vec::new(),
        };
        for builtin in BUILTINS.iter() {
            this.push(builtin.clone());
        }
        let mut office: Vec<Name> = office.into_iter().collect();
        office.sort();
        office.dedup();
        for name in office {
            this.push(Property {
                name: QualifiedName::unqualified(name),
                ty: PropertyType::Any,
                initial: Value::None,
                inherited: false,
                effect: Effect::Paint,
                group: GroupId::EXPORT,
                slot: 0,
            });
        }
        for component in host.into_iter().flat_map(ComponentHost::components) {
            let namespace = component.manifest.namespace();
            if component.declarations.properties.is_empty() {
                continue;
            }
            let group = GroupId(u16::try_from(this.groups.len()).expect("few groups"));
            this.groups.push(namespace.as_str().into());
            this.members.push(Vec::new());
            for decl in &component.declarations.properties {
                let PropertyDecl {
                    name,
                    ty,
                    initial,
                    inherited,
                    effect,
                } = decl.clone();
                this.push(Property {
                    name: QualifiedName::qualified(namespace.clone(), name),
                    ty,
                    initial,
                    inherited,
                    effect,
                    group,
                    slot: 0,
                });
            }
        }
        this.by_name.sort_by(|a, b| a.0.cmp(&b.0));
        this
    }

    fn push(&mut self, mut prop: Property) {
        let id = PropId(u32::try_from(self.props.len()).expect("few properties"));
        let members = &mut self.members[prop.group.index()];
        prop.slot = u16::try_from(members.len()).expect("few properties per group");
        members.push(id);
        self.by_name.push((prop.name.clone(), id));
        self.props.push(prop);
    }

    /// A property by its id.
    pub fn get(&self, id: PropId) -> &Property {
        &self.props[id.index()]
    }

    /// A property by its name: unqualified for a built-in property,
    /// qualified for a component's. Unqualified names of component
    /// properties resolve through the component host.
    pub fn find(&self, name: &QualifiedName) -> Option<PropId> {
        self.by_name
            .binary_search_by(|(n, _)| n.cmp(name))
            .ok()
            .map(|i| self.by_name[i].1)
    }

    /// A built-in property by its name. Panics if there is none: for
    /// names the code knows.
    pub fn builtin(&self, name: &str) -> PropId {
        let name = Name::new(name).expect("built-in property names are identifiers");
        self.find(&QualifiedName::unqualified(name))
            .expect("a built-in property")
    }

    /// Every property, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (PropId, &Property)> {
        self.props
            .iter()
            .enumerate()
            .map(|(i, p)| (PropId(i as u32), p))
    }

    /// The number of groups.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// The name of a group: `text`, `block`, or a component's namespace.
    pub fn group_name(&self, group: GroupId) -> &str {
        &self.groups[group.index()]
    }

    /// The properties of a group, in slot order.
    pub fn members(&self, group: GroupId) -> &[PropId] {
        &self.members[group.index()]
    }

    /// Whether a name is a property of the export group that the registry
    /// takes from the stylesheets that name it.
    pub fn is_office(name: &str) -> bool {
        name.strip_prefix("docx-")
            .or_else(|| name.strip_prefix("pptx-"))
            .is_some_and(|rest| !rest.is_empty())
    }
}

/// What a built-in property would do that no stage does yet, when a
/// declaration sets it to `value` (`None` for a render template): the
/// feature, for a warning that the declaration has no effect (SPEC §7.9).
/// Its initial value, and `placement: here`, which is where layout puts
/// every float, ask for nothing, nor does `marker-position: inside`, which
/// is where the text stage sets every marker. Each entry goes with the work
/// that reads the property: hanging markers with layout, `display`
/// with variants and visibility (SPEC §6.3), `template` with render
/// templates (§5.3), and the others with pages and running elements
/// (§7.1, §7.8), the baseline grid (§7.3), tables (§7.4), floats (§7.5),
/// margin notes (§7.6) and text wrap (§7.7). Properties of other targets
/// (slides, animation, export) and of drawings are not listed: a static or
/// PDF build ignores them by design. Nor is `width`: images read it, and
/// the text stage warns of it on the elements that do not
/// ([`crate::text`]).
pub(super) fn not_implemented(property: &Property, value: Option<&Value>) -> Option<&'static str> {
    if property.name.namespace.is_some() {
        return None;
    }
    if let Some(value) = value
        && (*value == property.initial
            || (property.name.name.as_str() == "placement"
                && matches!(value, Value::Keyword(k) if k.as_str() == "here"))
            || (property.name.name.as_str() == "marker-position"
                && matches!(value, Value::Keyword(k) if k.as_str() == "inside")))
    {
        return None;
    }
    Some(match property.name.name.as_str() {
        "display" | "show-only" => "showing and hiding content",
        // Markers are set inside the first line until layout hangs them.
        "marker-position" => "hanging markers",
        "trailer" => "block trailers",
        "column-tracks" => "tables",
        "grid-snap" | "baseline-grid" => "the baseline grid",
        "header" | "footer" | "content" => "page headers, footers and content",
        "margin-column" => "margin columns",
        "template" => "render templates",
        "placement" | "side" | "float-order" | "wrap" | "wrap-gap" | "wrap-shape" => {
            "placing floats and notes other than in the flow"
        }
        _ => return None,
    })
}

/// A hint for a name that is not a property but is often meant as one
/// (SPEC §5.1: one meaning per name).
pub(super) fn confusable(name: &str) -> Option<EcoString> {
    Some(match name {
        "columns" => "`columns` sets text columns and comes from the multicol component; \
                      table columns are `column-tracks`"
            .into(),
        "size" => "the font size is `font-size`; the page size is `page-size`".into(),
        "style" | "italic" => "use `font-style: italic`".into(),
        "weight" | "bold" => "use `font-weight: bold` or a number".into(),
        "colour" => "the property is spelt `color`".into(),
        "margin" => "set `margin-top`, `margin-bottom`, `margin-inner` and `margin-outer`".into(),
        "line-height" => "the distance between lines is `leading`".into(),
        "text-align" => "use `align`".into(),
        "text-indent" => "use `indent`".into(),
        _ if name.starts_with("font") => eco_format!(
            "the font properties are `font`, `font-size`, `font-weight` and `font-style`"
        ),
        _ => return None,
    })
}

const GROUP_NAMES: [&str; GroupId::BUILTIN as usize] = [
    "text",
    "paragraph",
    "block",
    "numbering",
    "visibility",
    "templates",
    "placement",
    "page",
    "graphics",
    "animation",
    "slides",
    "export",
    "math",
];

fn n(text: &str) -> Name {
    Name::new(text).expect("built-in names are identifiers")
}

fn kw(words: &[&str]) -> PropertyType {
    PropertyType::Keyword(words.iter().map(|w| n(w)).collect())
}

fn any_of(types: impl IntoIterator<Item = PropertyType>) -> PropertyType {
    PropertyType::AnyOf(types.into_iter().collect())
}

fn list(item: PropertyType) -> PropertyType {
    PropertyType::List(Box::new(item))
}

/// The built-in properties of SPEC §5.2, group by group.
static BUILTINS: LazyLock<Vec<Property>> = LazyLock::new(|| {
    use Effect::{Layout, Paint};
    use PropertyType::{Any, Auto, Bool, Int, Length, Number, Ratio, Str, Template};

    let mut out = Vec::new();
    let mut group = GroupId::TEXT;
    let mut inherited = true;
    let mut add =
        |name: &str, ty: PropertyType, initial: Value, effect: Effect, g: GroupId, i: bool| {
            out.push(Property {
                name: QualifiedName::unqualified(n(name)),
                ty,
                initial,
                inherited: i,
                effect,
                group: g,
                slot: 0,
            });
        };
    let pt = |v: f64| Value::Length(Abs::pt(v));
    let mm = |v: f64| Value::Length(Abs::mm(v));
    let word = |w: &str| Value::Keyword(n(w));

    // Text: inherited.
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop(
        "font",
        list(Str),
        Value::Array(EcoVec::from([Value::Str("New Computer Modern".into())])),
        Layout,
    );
    prop("font-size", Length, pt(11.0), Layout);
    prop(
        "font-weight",
        any_of([Int, kw(&["normal", "bold"])]),
        Value::Int(400),
        Layout,
    );
    prop(
        "font-style",
        kw(&["normal", "italic", "oblique"]),
        word("normal"),
        Layout,
    );
    prop("tracking", Length, pt(0.0), Layout);
    prop(
        "features",
        any_of([PropertyType::None, list(Str)]),
        Value::None,
        Layout,
    );
    prop("lang", any_of([Auto, Str]), Value::Auto, Layout);
    prop(
        "color",
        PropertyType::Color,
        Value::Color(Color::BLACK),
        Paint,
    );

    // Paragraph: inherited, so a section's paragraphs follow it.
    group = GroupId::PARAGRAPH;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop("leading", Length, Value::Em(Em::new(0.65)), Layout);
    prop("indent", Length, pt(0.0), Layout);
    prop(
        "align",
        kw(&["start", "center", "end"]),
        word("start"),
        Layout,
    );
    prop("justify", Bool, Value::Bool(true), Layout);
    prop("hyphenate", any_of([Auto, Bool]), Value::Auto, Layout);
    // SPEC §7.2: punctuation and hyphens hang into the margin.
    prop("protrusion", Bool, Value::Bool(false), Layout);
    prop("widows", Int, Value::Int(2), Layout);
    prop("orphans", Int, Value::Int(2), Layout);
    prop("grid-snap", Bool, Value::Bool(true), Layout);

    // Box properties from here on: not inherited.
    inherited = false;
    group = GroupId::BLOCK;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop("space-before", Length, pt(0.0), Layout);
    prop("space-after", Length, pt(0.0), Layout);
    prop("padding", any_of([Length, list(Length)]), pt(0.0), Layout);
    prop("border", Any, Value::None, Layout);
    // `auto` takes `border`'s value; `none` draws no border on the side.
    for side in ["border-top", "border-right", "border-bottom", "border-left"] {
        prop(side, Any, Value::Auto, Layout);
    }
    prop(
        "background",
        any_of([PropertyType::None, PropertyType::Color]),
        Value::None,
        Paint,
    );
    prop("keep-with-next", Bool, Value::Bool(false), Layout);
    prop("keep-together", Bool, Value::Bool(false), Layout);
    prop(
        "break-before",
        any_of([Auto, kw(&["page", "column", "odd-page", "even-page"])]),
        Value::Auto,
        Layout,
    );
    prop("breakable", Bool, Value::Bool(true), Layout);
    prop("trailer", Template, Value::None, Layout);
    prop(
        "width",
        any_of([Auto, Length, Ratio, Number]),
        Value::Auto,
        Layout,
    );

    group = GroupId::NUMBERING;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    // One meaning per name (SPEC §5.1): a pattern string such as "1.".
    prop(
        "numbering",
        any_of([PropertyType::None, Str]),
        Value::None,
        Layout,
    );
    // The counters to restart, by name: `figure`, `"exam.question"`. A
    // qualified name is not a keyword, so the Resolve stage checks the
    // names (SPEC §4.5).
    prop("counter-reset", Any, Value::None, Layout);
    // `auto`: the element's number, or a figure's word and number; `none`;
    // or a string set in its place, such as "•".
    prop(
        "marker",
        any_of([Auto, PropertyType::None, Str]),
        Value::Auto,
        Layout,
    );
    // `inside` sets the marker in the first line; `hanging 8mm` hangs it
    // before the text (not laid out yet).
    prop(
        "marker-position",
        any_of([
            Auto,
            kw(&["inside", "hanging"]),
            list(any_of([kw(&["hanging"]), Length])),
        ]),
        Value::Auto,
        Layout,
    );
    // `auto`: the reference word and the number (`Figure 2.1`); `number`:
    // the number alone.
    prop(
        "ref-format",
        any_of([Auto, kw(&["number"])]),
        Value::Auto,
        Layout,
    );

    group = GroupId::VISIBILITY;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop(
        "display",
        any_of([
            Auto,
            kw(&["block", "inline", "none", "hidden", "answer-space"]),
            // SPEC §6.3: `placeholder("Solution in the mark scheme")`.
            PropertyType::Call(EcoVec::from([n("placeholder")])),
        ]),
        Value::Auto,
        Layout,
    );
    prop("show-only", Any, Value::None, Layout);

    group = GroupId::TEMPLATES;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop("template", Any, Value::Auto, Layout);

    group = GroupId::PLACEMENT;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop(
        "placement",
        any_of([
            Auto,
            kw(&["here", "top", "bottom", "page", "wrap", "margin"]),
        ]),
        Value::Auto,
        Layout,
    );
    prop(
        "side",
        any_of([Auto, kw(&["inner", "outer", "left", "right"])]),
        Value::Auto,
        Layout,
    );
    prop("float-order", Any, Value::Auto, Layout);
    prop("wrap", Any, Value::None, Layout);
    prop("wrap-gap", Length, pt(6.0), Layout);
    prop("wrap-shape", Any, Value::Auto, Layout);
    prop("column-tracks", Any, Value::Auto, Layout);

    group = GroupId::PAGE;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop(
        "page-size",
        any_of([kw(&["A3", "A4", "A5", "letter", "legal"]), list(Length)]),
        word("A4"),
        Layout,
    );
    for side in [
        "margin-top",
        "margin-bottom",
        "margin-inner",
        "margin-outer",
    ] {
        prop(side, Length, mm(20.0), Layout);
    }
    prop("margin-column", Any, Value::None, Layout);
    prop(
        "baseline-grid",
        any_of([PropertyType::None, Length]),
        Value::None,
        Layout,
    );
    prop("header", Template, Value::None, Layout);
    prop("footer", Template, Value::None, Layout);
    prop("content", Template, Value::None, Layout);

    // Graphics inherit within a drawing and only repaint (SPEC §10.2).
    group = GroupId::GRAPHICS;
    let mut prop = |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, true);
    prop("stroke", Any, Value::Auto, Paint);
    prop(
        "stroke-color",
        any_of([Auto, PropertyType::Color]),
        Value::Auto,
        Paint,
    );
    prop("stroke-width", any_of([Auto, Length]), Value::Auto, Paint);
    prop(
        "fill",
        any_of([PropertyType::None, Auto, PropertyType::Color]),
        Value::Auto,
        Paint,
    );
    prop("symbol", Any, Value::Auto, Paint);
    prop("head", Any, Value::None, Paint);
    prop("palette", Any, Value::Auto, Paint);

    // Effects are render transforms, so paint (SPEC §10.2).
    group = GroupId::ANIMATION;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    for name in [
        "entrance",
        "exit",
        "emphasis-effect",
        "state-effect",
        "transition",
    ] {
        prop(name, Any, Value::None, Paint);
    }
    prop(
        "duration",
        any_of([Auto, PropertyType::Duration]),
        Value::Auto,
        Paint,
    );

    group = GroupId::SLIDES;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    // SPEC §10.1: `reveal: true` shows a list's items one per step.
    prop("reveal", Bool, Value::Bool(false), Layout);
    prop("animation", Any, Value::None, Layout);
    for name in [
        "live-static",
        "overflow",
        "reserve",
        "fallback",
        "slide-role",
        "slide-layout",
        "slide-overflow",
    ] {
        prop(name, Any, Value::Auto, Layout);
    }
    prop("slide-break", Any, Value::None, Layout);
    prop(
        "slide-fit-margin",
        any_of([Auto, Length]),
        Value::Auto,
        Layout,
    );

    // Export properties do not change Folio's own output.
    group = GroupId::EXPORT;
    let mut prop =
        |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, inherited);
    prop(
        "export-name",
        any_of([PropertyType::None, Str]),
        Value::None,
        Paint,
    );
    prop("export-as", Any, Value::Auto, Paint);

    // Math (SPEC §8.1): inherited, like text properties, so that a
    // section or the whole document can set them.
    group = GroupId::MATH;
    let mut prop = |name: &str, ty, initial, effect| add(name, ty, initial, effect, group, true);
    // The families formulas are set in (SPEC §8.2), the first available
    // first; New Computer Modern Math, which is bundled, by default.
    prop(
        "math-font",
        list(Str),
        Value::Array(EcoVec::from([Value::Str(
            "New Computer Modern Math".into(),
        )])),
        Layout,
    );
    // How a vector is set: `math.vector { math-style: bold-upright; }`.
    // `auto` is bold upright; `arrow` an italic letter under an arrow. A
    // unit vector takes its vector's style with a hat in place of the
    // arrow.
    prop(
        "math-style",
        any_of([Auto, kw(&["bold-upright", "bold-italic", "arrow"])]),
        Value::Auto,
        Layout,
    );
    // The decimal marker of quantities and numbers: `auto` follows the
    // language of the text (a point in English, a comma in Polish).
    prop(
        "decimal-marker",
        any_of([Auto, kw(&["point", "comma"])]),
        Value::Auto,
        Layout,
    );

    for p in &out {
        debug_assert!(p.ty.accepts(&p.initial), "initial value of `{}`", p.name);
    }
    out
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_unique_and_grouped() {
        let props = Properties::new(None, [n("docx-style"), n("docx-style")]);
        let names: std::collections::HashSet<String> =
            props.iter().map(|(_, p)| p.name.to_string()).collect();
        assert_eq!(names.len(), props.iter().count(), "names are unique");
        let size = props.get(props.builtin("font-size"));
        assert!(size.inherited);
        assert_eq!(size.group, GroupId::TEXT);
        let color = props.get(props.builtin("color"));
        assert_eq!(color.effect, Effect::Paint);
        let space = props.get(props.builtin("space-before"));
        assert!(!space.inherited);
        assert_eq!(space.effect, Effect::Layout);
        let docx = props.get(props.builtin("docx-style"));
        assert_eq!(docx.group, GroupId::EXPORT);
        for (_, p) in props.iter() {
            assert!(p.ty.accepts(&p.initial), "{}", p.name);
            assert_eq!(
                props.members(p.group)[p.slot as usize],
                props.find(&p.name).unwrap()
            );
        }
        assert!(Properties::is_office("pptx-placeholder"));
        assert!(!Properties::is_office("docx-"));
        assert!(confusable("columns").unwrap().contains("column-tracks"));
    }
}

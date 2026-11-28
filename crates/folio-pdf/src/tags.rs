//! The structure tree and the outline (SPEC §11.1): which structure element
//! each glyph run is content of, and the bookmarks of headings.
//!
//! The writer walks each page's fragments in painting order. A group is a
//! box of a node of the [`Tag`] tree: of a child of the node whose box
//! holds it, found by its location (source and instance) among the
//! children not yet entered, in document order (a footnote's note, whose box
//! comes after its anchor's line, also among those before the last child
//! entered, which leaves that child the last entered); or, at the top of a page,
//! the child last entered if the group has its location, which continues
//! from the page before. The instance tells apart the blocks that a loop
//! or the calls of a closure make from one source. A group that matches
//! no child, such as a page's body region, belongs to the node around it.
//!
//! The tree written follows the input with three kinds of elements added: a
//! `P` around text that a node holds directly where PDF expects no text
//! (in a `Sect` or `Div`), the `LBody` of a list item, around all it
//! holds but its label, and a `Link` around the text of each link, with
//! its annotations (one for each page it is on) first, as PDF/UA-1 asks
//! (§7.18.5). A heading is `H1` to `H6` by the sections around
//! it, so levels never skip, as PDF/UA-1 asks (§7.4.2), whatever levels
//! the source wrote.

use std::collections::HashMap;
use std::num::NonZeroU16;

use ecow::EcoString;
use folio_component::{Abs, Location, SourceRef};
use krilla::destination::XyzDestination;
use krilla::outline::{Outline, OutlineNode};
use krilla::tagging::{
    Identifier, ListNumbering, Node, TableHeaderScope, Tag as KTag, TagGroup, TagKind, TagTree,
};

use crate::{ListKind, Role, Tag, pt};

/// Builds the structure tree as the pages are drawn.
pub(crate) struct Tagger<'a> {
    /// The nodes of the tree written, the top first.
    nodes: Vec<Out<'a>>,
    /// The page being drawn.
    page: usize,
    /// Whether nothing has been drawn on the page yet but the boxes around
    /// the current one, so that a box may continue one from the page
    /// before.
    leading: bool,
    /// The headings, in order.
    headings: Vec<Heading>,
    /// The sources of headings with content but no text, which cannot
    /// have a title.
    untitled: Vec<SourceRef>,
    /// The sources of figures with content but no alternate description.
    undescribed: Vec<SourceRef>,
    /// The `Link` node of each run of a link's glyphs, by the run's index.
    links: Vec<Option<usize>>,
    /// The nodes of the input that are footnotes' notes ([`Tag::foot`]),
    /// by source, with their instances.
    feet: HashMap<SourceRef, Vec<u64>>,
}

/// What the tagger gives: the tag tree, the headings for the outline, and
/// the nodes PDF/UA-1 does not allow as they are.
pub(crate) struct Finished {
    pub(crate) tree: TagTree,
    pub(crate) headings: Vec<Heading>,
    pub(crate) missing: Missing,
}

/// The nodes of a tag tree that lack what PDF/UA-1 asks of them, by their
/// sources, which krilla's validator does not name.
#[derive(Debug, Clone, Default)]
pub(crate) struct Missing {
    /// Headings with content but no text.
    pub(crate) untitled: Vec<SourceRef>,
    /// Figures with content but no alternate description.
    pub(crate) undescribed: Vec<SourceRef>,
}

/// A heading for the outline: its level, text, page and top.
pub(crate) type Heading = (u16, EcoString, usize, f32);

/// A node of the tree being written.
struct Out<'a> {
    kind: Kind<'a>,
    /// What it holds, in order.
    kids: Vec<Kid>,
    /// The nodes of the input it can hold: the children of its input node.
    children: &'a [Tag],
    /// The child it entered last, by index into `children`.
    cursor: Option<usize>,
    /// The node written for each child entered.
    entered: Vec<Option<usize>>,
    /// The `P` that holds the text it holds directly, if it needs one.
    text: Option<usize>,
    /// The `LBody` of a list item.
    body: Option<usize>,
    /// How many sections it is in, itself included.
    sections: u16,
    /// For a heading with text, where its first box is: its page and top.
    top: Option<(usize, f32)>,
    /// The node it is a kid of, if any.
    parent: Option<usize>,
    /// Whether it or a node inside it holds content, so that it is in the
    /// tree written.
    shows: bool,
}

/// What a node written is.
#[derive(Copy, Clone)]
enum Kind<'a> {
    /// The node above the document's, which pages and their regions are
    /// boxes of.
    Top,
    /// A node of the input.
    Input(&'a Tag),
    /// A paragraph around text that its parent holds directly.
    Text,
    /// The body of a list item.
    ListBody,
    /// A link: its annotations and the content they cover.
    Link,
}

enum Kid {
    Node(usize),
    Content(Identifier),
    /// An annotation, which a `Link` holds.
    Annotation(Identifier),
}

impl<'a> Tagger<'a> {
    /// The node above the document's.
    pub(crate) const TOP: usize = 0;

    pub(crate) fn new(root: &'a Tag) -> Self {
        let mut tagger = Self {
            nodes: Vec::new(),
            page: 0,
            leading: true,
            headings: Vec::new(),
            untitled: Vec::new(),
            undescribed: Vec::new(),
            links: Vec::new(),
            feet: HashMap::new(),
        };
        fn feet(tag: &Tag, out: &mut HashMap<SourceRef, Vec<u64>>) {
            if tag.foot {
                out.entry(tag.source).or_default().push(tag.instance);
            }
            for child in &tag.children {
                feet(child, out);
            }
        }
        feet(root, &mut tagger.feet);
        tagger.push(Kind::Top, std::slice::from_ref(root), 0, None);
        tagger
    }

    fn push(
        &mut self,
        kind: Kind<'a>,
        children: &'a [Tag],
        sections: u16,
        parent: Option<usize>,
    ) -> usize {
        self.nodes.push(Out {
            kind,
            kids: Vec::new(),
            children,
            cursor: None,
            entered: vec![None; children.len()],
            text: None,
            body: None,
            sections,
            top: None,
            parent,
            shows: false,
        });
        self.nodes.len() - 1
    }

    /// Starts a page.
    pub(crate) fn start_page(&mut self, page: usize) {
        self.page = page;
        self.leading = true;
    }

    /// Notes that the walk has passed content or the end of a box, so the
    /// boxes that follow on the page start there.
    pub(crate) fn advance(&mut self) {
        self.leading = false;
    }

    /// The node a group of the element instance at `location` within a
    /// box of `node` is a box of, with its top at `y` on the page.
    pub(crate) fn enter(&mut self, node: usize, location: Location, y: Abs) -> usize {
        let out = &self.nodes[node];
        let children = out.children;
        // A box continues the node last entered at the top of a page, and
        // a formula's next chunk, on a later line, continues the formula.
        if let Some(last) = out.cursor
            && (self.leading || children[last].role == Role::Formula)
            && is_box_of(&children[last], location)
            && let Some(entered) = out.entered[last]
        {
            return entered;
        }
        let from = out.cursor.map_or(0, |last| last + 1);
        let found = children[from..]
            .iter()
            .position(|child| is_box_of(child, location))
            .map(|i| from + i);
        // A row of a table has no box: a cell's box is found among the
        // children of the rows from the current one on, and enters its row
        // first, or the row it entered before, as the cells of a row broken
        // across pages continue it (SPEC §7.4).
        if found.is_none() {
            let rows = out.cursor.unwrap_or(0);
            let row = children[rows..]
                .iter()
                .position(|child| {
                    child.role == Role::TableRow
                        && child.children.iter().any(|cell| is_box_of(cell, location))
                })
                .map(|i| rows + i);
            if let Some(index) = row {
                let entered = match self.nodes[node].entered[index] {
                    Some(entered) => {
                        self.nodes[node].cursor = Some(index);
                        entered
                    }
                    None => self.open(node, index, true, y),
                };
                return self.enter_cell(entered, location, y);
            }
        }
        let out = &self.nodes[node];
        // A footnote's box comes after its anchor's line, whose formulas
        // after the anchor the cursor has passed: it is found among the
        // notes not yet entered before the cursor too, and leaves the
        // cursor where it is (SPEC §7.8).
        let behind = || {
            children[..from].iter().enumerate().position(|(i, child)| {
                child.foot && out.entered[i].is_none() && is_box_of(child, location)
            })
        };
        let (index, moves) = match found {
            Some(index) => (index, true),
            None => match behind() {
                Some(index) => (index, false),
                None => {
                    // A note anchored deeper than its box is drawn, as in a
                    // cell of a table, is found through the nodes entered
                    // that hold it (SPEC §7.4).
                    if self.is_foot_box(location)
                        && let Some(inner) =
                            children
                                .iter()
                                .zip(&out.entered)
                                .find_map(|(child, entered)| {
                                    entered.filter(|_| {
                                        !is_box_of(child, location) && holds(child, location)
                                    })
                                })
                    {
                        return self.enter(inner, location, y);
                    }
                    return node;
                }
            },
        };
        self.open(node, index, moves, y)
    }

    /// The node of a cell of the row `row`: the one entered before, as the
    /// cells of a row broken across pages continue on the next, or a new
    /// one.
    fn enter_cell(&mut self, row: usize, location: Location, y: Abs) -> usize {
        let out = &self.nodes[row];
        if let Some(index) = out
            .children
            .iter()
            .position(|cell| is_box_of(cell, location))
        {
            if let Some(entered) = out.entered[index] {
                return entered;
            }
            return self.open(row, index, true, y);
        }
        self.enter(row, location, y)
    }

    /// Opens a node for child `index` of `node`, in document order; `moves`
    /// moves the cursor of `node` to it.
    fn open(&mut self, node: usize, index: usize, moves: bool, y: Abs) -> usize {
        let out = &self.nodes[node];
        let children = out.children;
        let tag = &children[index];
        let sections = out.sections + u16::from(tag.role == Role::Section);
        let parent = self.container(node, tag.role);
        let child = self.push(Kind::Input(tag), &tag.children, sections, Some(parent));
        // A note found behind the cursor goes before the nodes entered
        // after it in document order, so the tree keeps the source's order.
        let later: Vec<usize> = self.nodes[node].entered[index + 1..]
            .iter()
            .flatten()
            .copied()
            .collect();
        let kids = &mut self.nodes[parent].kids;
        let at = kids
            .iter()
            .position(|kid| matches!(kid, Kid::Node(n) if later.contains(n)))
            .filter(|_| !moves)
            .unwrap_or(kids.len());
        kids.insert(at, Kid::Node(child));
        let out = &mut self.nodes[node];
        if moves {
            out.cursor = Some(index);
        }
        out.entered[index] = Some(child);
        if tag.role == Role::Heading {
            self.nodes[child].top = Some((self.page, pt(y)));
        }
        child
    }

    /// Whether `node` is a footnote's note ([`Tag::foot`]), whose boxes are
    /// drawn after the rest of their page.
    pub(crate) fn is_foot(&self, node: usize) -> bool {
        matches!(self.nodes[node].kind, Kind::Input(tag) if tag.foot)
    }

    /// Whether a box of the element instance at `location` is a footnote's
    /// note, whether or not its node could be entered.
    pub(crate) fn is_foot_box(&self, location: Location) -> bool {
        self.feet
            .get(&location.source)
            .is_some_and(|instances| instances.contains(&location.instance))
    }

    /// Adds marked content to `node`: to the `Link` of run `link` in it,
    /// if it is a link's text.
    pub(crate) fn content(&mut self, node: usize, id: Identifier, link: Option<usize>) {
        let target = self.text_target(node);
        self.note_heading(target);
        self.note_figure(target);
        let Some(link) = link else {
            self.nodes[target].kids.push(Kid::Content(id));
            return;
        };
        if self.links.len() <= link {
            self.links.resize(link + 1, None);
        }
        let out = match self.links[link] {
            Some(out) => out,
            None => {
                let sections = self.nodes[target].sections;
                let out = self.push(Kind::Link, &[], sections, Some(target));
                self.nodes[target].kids.push(Kid::Node(out));
                self.links[link] = Some(out);
                out
            }
        };
        self.nodes[out].kids.push(Kid::Content(id));
    }

    /// Adds an annotation of run `link` to its `Link`, after the
    /// annotations it has, before its content.
    pub(crate) fn annotation(&mut self, link: usize, id: Identifier) {
        let Some(Some(out)) = self.links.get(link).copied() else {
            return;
        };
        let kids = &mut self.nodes[out].kids;
        let at = kids
            .iter()
            .position(|kid| !matches!(kid, Kid::Annotation(_)))
            .unwrap_or(kids.len());
        kids.insert(at, Kid::Annotation(id));
    }

    /// Notes the headings for the outline that `target`, which content is
    /// about to be added to, shows for the first time: `target` itself or
    /// a node around it, such as the heading around a formula or an image.
    /// A heading is in the outline once it shows something, as it is in
    /// the tree only then; without text, it cannot have a title.
    fn note_heading(&mut self, target: usize) {
        let mut at = Some(target);
        // The nodes around one that shows show too, so the walk stops at
        // the first that does.
        while let Some(node) = at {
            let out = &mut self.nodes[node];
            if out.shows {
                break;
            }
            out.shows = true;
            at = out.parent;
            if let Kind::Input(tag) = out.kind
                && tag.role == Role::Heading
            {
                match (tag.text.as_ref().filter(|t| !t.is_empty()), out.top) {
                    (Some(text), Some((page, y))) => {
                        let level = out.sections.clamp(1, 6);
                        self.headings.push((level, text.clone(), page, y));
                    }
                    _ => self.untitled.push(tag.source),
                }
            }
        }
    }

    /// Notes a figure without an alternate description when `target`,
    /// which content is about to be added to, is one that shows nothing
    /// yet.
    fn note_figure(&mut self, target: usize) {
        let out = &self.nodes[target];
        if let Kind::Input(tag) = out.kind
            && tag.role == Role::Figure
            && out.kids.is_empty()
            && tag.text.as_ref().is_none_or(|t| t.trim().is_empty())
        {
            self.undescribed.push(tag.source);
        }
    }

    /// Where a child of `role` of a node goes: into the node, or for a
    /// list item's body, into its `LBody`.
    fn container(&mut self, node: usize, role: Role) -> usize {
        let Kind::Input(tag) = self.nodes[node].kind else {
            return node;
        };
        if tag.role != Role::ListItem || role == Role::Label {
            return node;
        }
        if let Some(body) = self.nodes[node].body {
            return body;
        }
        let sections = self.nodes[node].sections;
        let body = self.push(Kind::ListBody, &[], sections, Some(node));
        self.nodes[node].kids.push(Kid::Node(body));
        self.nodes[node].body = Some(body);
        body
    }

    /// Where text that `node` holds directly goes: into the node if PDF
    /// allows text there, else into a `P` of its own.
    fn text_target(&mut self, node: usize) -> usize {
        let holds_text = match self.nodes[node].kind {
            Kind::Text => true,
            Kind::Input(tag) => matches!(
                tag.role,
                Role::Title
                    | Role::Heading
                    | Role::Paragraph
                    | Role::Label
                    | Role::Caption
                    | Role::Code
                    | Role::Formula
                    | Role::Figure
                    | Role::TableHeader(_)
                    | Role::TableCell(_)
            ),
            Kind::Top | Kind::ListBody | Kind::Link => false,
        };
        if holds_text {
            return node;
        }
        if let Some(text) = self.nodes[node].text {
            return text;
        }
        let parent = self.container(node, Role::Paragraph);
        let sections = self.nodes[node].sections;
        let text = self.push(Kind::Text, &[], sections, Some(parent));
        self.nodes[parent].kids.push(Kid::Node(text));
        self.nodes[node].text = Some(text);
        text
    }

    /// The tag tree, in the document's language, the headings for the
    /// outline, and what PDF/UA-1 does not allow in it.
    pub(crate) fn finish(self, lang: Option<&str>) -> Finished {
        let mut tree = TagTree::new().with_lang(lang.map(String::from));
        let mut taken: Vec<Option<Out<'a>>> = self.nodes.into_iter().map(Some).collect();
        // The document's node is the tree's root `Document`; content the
        // top holds directly (none, from the page builder) goes in a `P`.
        for kid in take(&mut taken, Self::TOP).kids {
            match kid {
                Kid::Node(node) => {
                    let out = take(&mut taken, node);
                    if matches!(out.kind, Kind::Input(tag) if tag.role == Role::Document) {
                        for kid in out.kids {
                            if let Some(node) = kid_node(kid, &mut taken) {
                                tree.push(node);
                            }
                        }
                    } else if let Some(group) = group(out, &mut taken) {
                        tree.push(group);
                    }
                }
                Kid::Content(id) | Kid::Annotation(id) => {
                    tree.push(TagGroup::with_children(KTag::P, vec![id.into()]));
                }
            }
        }
        Finished {
            tree,
            headings: self.headings,
            missing: Missing {
                untitled: self.untitled,
                undescribed: self.undescribed,
            },
        }
    }
}

/// A cell's span as PDF writes it: only where it is more than one.
fn span_of(n: u32) -> Option<std::num::NonZeroU32> {
    std::num::NonZeroU32::new(n).filter(|n| n.get() > 1)
}

/// Whether a tag or one of its descendants is a box of the element
/// instance at `location`.
fn holds(tag: &Tag, location: Location) -> bool {
    is_box_of(tag, location) || tag.children.iter().any(|c| holds(c, location))
}

/// Whether a group of the element instance at `location` is a box of
/// `tag`: they name the same source and the same instance.
fn is_box_of(tag: &Tag, location: Location) -> bool {
    tag.source == location.source && tag.instance == location.instance
}

fn take<'a>(nodes: &mut [Option<Out<'a>>], index: usize) -> Out<'a> {
    nodes[index].take().expect("a node is written once")
}

fn kid_node(kid: Kid, nodes: &mut [Option<Out<'_>>]) -> Option<Node> {
    match kid {
        Kid::Node(index) => group(take(nodes, index), nodes).map(Node::Group),
        Kid::Content(id) | Kid::Annotation(id) => Some(Node::Leaf(id)),
    }
}

/// A node as a tag group, or `None` if it holds nothing.
fn group(out: Out<'_>, nodes: &mut [Option<Out<'_>>]) -> Option<TagGroup> {
    let tag: TagKind = match out.kind {
        Kind::Top => KTag::Div.into(),
        Kind::Text => KTag::P.into(),
        Kind::ListBody => KTag::LBody.into(),
        Kind::Link => KTag::Link.into(),
        Kind::Input(tag) => match tag.role {
            Role::Document | Role::Group => KTag::Div.into(),
            Role::Title => KTag::Title.into(),
            Role::Section => KTag::Section.into(),
            Role::Heading => {
                let level = NonZeroU16::new(out.sections.clamp(1, 6)).expect("at least one");
                let title = tag.text.as_ref().map(|t| t.to_string());
                KTag::Hn(level, title).into()
            }
            Role::Paragraph => KTag::P.into(),
            Role::List(kind) => KTag::L(match kind {
                ListKind::Bulleted => ListNumbering::Disc,
                ListKind::Numbered => ListNumbering::Decimal,
                ListKind::Terms => ListNumbering::None,
            })
            .into(),
            Role::ListItem => KTag::LI.into(),
            Role::Label => KTag::Lbl.into(),
            Role::Caption => KTag::Caption.into(),
            Role::Note => KTag::Note.into(),
            Role::Code => KTag::Code.into(),
            Role::Formula => KTag::Formula(tag.text.as_ref().map(|t| t.to_string())).into(),
            Role::Figure => KTag::Figure(tag.text.as_ref().map(|t| t.to_string())).into(),
            Role::Table => KTag::Table.into(),
            Role::TableRow => KTag::TR.into(),
            Role::TableHeader(span) => KTag::TH(TableHeaderScope::Column)
                .with_row_span(span_of(span.rows))
                .with_col_span(span_of(span.columns))
                .into(),
            Role::TableCell(span) => KTag::TD
                .with_row_span(span_of(span.rows))
                .with_col_span(span_of(span.columns))
                .into(),
        },
    };
    let children: Vec<Node> = out
        .kids
        .into_iter()
        .filter_map(|kid| kid_node(kid, nodes))
        .collect();
    (!children.is_empty()).then(|| TagGroup::with_children(tag, children))
}

/// The outline: a bookmark for each heading, nested by level.
pub(crate) fn outline(headings: Vec<Heading>) -> Outline {
    let mut outline = Outline::new();
    // The open bookmarks, outermost first.
    let mut open: Vec<(u16, OutlineNode)> = Vec::new();
    let close = |open: &mut Vec<(u16, OutlineNode)>, outline: &mut Outline| {
        let (_, node) = open.pop().expect("a bookmark is open");
        match open.last_mut() {
            Some((_, parent)) => parent.push_child(node),
            None => outline.push_child(node),
        }
    };
    for (level, text, page, y) in headings {
        while open.last().is_some_and(|(l, _)| *l >= level) {
            close(&mut open, &mut outline);
        }
        let destination = XyzDestination::new(page, krilla::geom::Point::from_xy(0.0, y));
        open.push((level, OutlineNode::new(text.to_string(), destination)));
    }
    while !open.is_empty() {
        close(&mut open, &mut outline);
    }
    outline
}

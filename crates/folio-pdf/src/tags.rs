//! The structure tree and the outline (SPEC §11.1): which structure element
//! each glyph run is content of, and the bookmarks of headings.
//!
//! The writer walks each page's fragments in painting order. A group is a
//! box of a node of the [`Tag`] tree: of a child of the node whose box
//! holds it, found by its location (source and instance) among the
//! children not yet entered, in document order; or, at the top of a page,
//! the child last entered if the group has its location, which continues
//! from the page before. The instance tells apart the blocks that a loop
//! or the calls of a closure make from one source. A group that matches no child, such as
//! a page's body region, belongs to the node around it.
//!
//! The tree written follows the input with two kinds of elements added: a
//! `P` around text that a node holds directly where PDF expects no text
//! (in a `Sect` or `Div`), and the `LBody` of a list item, around all it
//! holds but its label. A heading is `H1` to `H6` by the sections around
//! it, so levels never skip, as PDF/UA-1 asks (§7.4.2), whatever levels
//! the source wrote.

use std::num::NonZeroU16;

use ecow::EcoString;
use folio_component::{Abs, Location, SourceRef};
use krilla::destination::XyzDestination;
use krilla::outline::{Outline, OutlineNode};
use krilla::tagging::{Identifier, ListNumbering, Node, Tag as KTag, TagGroup, TagKind, TagTree};

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
}

enum Kid {
    Node(usize),
    Content(Identifier),
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
        };
        tagger.push(Kind::Top, std::slice::from_ref(root), 0);
        tagger
    }

    fn push(&mut self, kind: Kind<'a>, children: &'a [Tag], sections: u16) -> usize {
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
        if self.leading
            && let Some(last) = out.cursor
            && is_box_of(&children[last], location)
            && let Some(entered) = out.entered[last]
        {
            return entered;
        }
        let from = out.cursor.map_or(0, |last| last + 1);
        let Some(index) = children[from..]
            .iter()
            .position(|child| is_box_of(child, location))
            .map(|i| from + i)
        else {
            return node;
        };
        let tag = &children[index];
        let sections = out.sections + u16::from(tag.role == Role::Section);
        let parent = self.container(node, tag.role);
        let child = self.push(Kind::Input(tag), &tag.children, sections);
        self.nodes[parent].kids.push(Kid::Node(child));
        let out = &mut self.nodes[node];
        out.cursor = Some(index);
        out.entered[index] = Some(child);
        if tag.role == Role::Heading {
            self.nodes[child].top = Some((self.page, pt(y)));
        }
        child
    }

    /// Adds marked content to `node`.
    pub(crate) fn content(&mut self, node: usize, id: Identifier) {
        let target = self.text_target(node);
        let out = &mut self.nodes[target];
        // A heading is in the outline once it shows something, as it is
        // in the tree only then.
        if let Kind::Input(tag) = out.kind
            && tag.role == Role::Heading
            && out.kids.is_empty()
        {
            match (tag.text.as_ref().filter(|t| !t.is_empty()), out.top) {
                (Some(text), Some((page, y))) => {
                    let level = out.sections.clamp(1, 6);
                    self.headings.push((level, text.clone(), page, y));
                }
                _ => self.untitled.push(tag.source),
            }
        }
        out.kids.push(Kid::Content(id));
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
        let body = self.push(Kind::ListBody, &[], sections);
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
            ),
            Kind::Top | Kind::ListBody => false,
        };
        if holds_text {
            return node;
        }
        if let Some(text) = self.nodes[node].text {
            return text;
        }
        let parent = self.container(node, Role::Paragraph);
        let sections = self.nodes[node].sections;
        let text = self.push(Kind::Text, &[], sections);
        self.nodes[parent].kids.push(Kid::Node(text));
        self.nodes[node].text = Some(text);
        text
    }

    /// The tag tree, in the document's language, the headings for the
    /// outline, and the sources of headings without text.
    pub(crate) fn finish(self, lang: Option<&str>) -> (TagTree, Vec<Heading>, Vec<SourceRef>) {
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
                Kid::Content(id) => tree.push(TagGroup::with_children(KTag::P, vec![id.into()])),
            }
        }
        (tree, self.headings, self.untitled)
    }
}

/// Whether a group of the element instance at `location` is a box of
/// `tag`: they name the same source and the same instance, or one of them
/// does not know which instance it is ([`Location::UNKNOWN_INSTANCE`]).
fn is_box_of(tag: &Tag, location: Location) -> bool {
    tag.source == location.source
        && (tag.instance == location.instance
            || tag.instance == Location::UNKNOWN_INSTANCE
            || location.instance == Location::UNKNOWN_INSTANCE)
}

fn take<'a>(nodes: &mut [Option<Out<'a>>], index: usize) -> Out<'a> {
    nodes[index].take().expect("a node is written once")
}

fn kid_node(kid: Kid, nodes: &mut [Option<Out<'_>>]) -> Option<Node> {
    match kid {
        Kid::Node(index) => group(take(nodes, index), nodes).map(Node::Group),
        Kid::Content(id) => Some(Node::Leaf(id)),
    }
}

/// A node as a tag group, or `None` if it holds nothing.
fn group(out: Out<'_>, nodes: &mut [Option<Out<'_>>]) -> Option<TagGroup> {
    let tag: TagKind = match out.kind {
        Kind::Top => KTag::Div.into(),
        Kind::Text => KTag::P.into(),
        Kind::ListBody => KTag::LBody.into(),
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

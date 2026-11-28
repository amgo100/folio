//! The two short ways to write a grid's cells (SPEC §7.4): pipe rows in
//! its body (`| a | b |`) and data rows from a value (`rows:`, `header:`).
//! Both make the same `cell` elements that `#cell[..]` makes, so the
//! document model and layout see one kind of grid.

use ecow::{EcoString, eco_format};
use folio_component::QualifiedName;
use folio_syntax::Span;
use folio_syntax::ast::{self, AstNode};

use super::content::{Content, Node};
use super::element::canonical_fields;
use super::error::{EvalError, EvalResult, bail};
use super::ops::a_type;
use super::value::{Arg, Args, Value};
use super::vm::Vm;
use crate::model::schema;
use crate::text::MAX_TRACKS;

/// The field a `|---|` line sets.
const HEADER_ROWS: &str = "header-rows";

/// The name of the argument that carries a `|---|` line's header rows to
/// instantiation ([`separator_arg`]): no identifier is spelled so.
const SEPARATOR: &str = "|---|";

/// Whether an element function is the built-in `grid`.
pub(super) fn is_grid(name: &QualifiedName) -> bool {
    name.namespace.is_none() && name.name.as_str() == "grid"
}

/// The bytes a node of content counts against the memory limit here.
const NODE_BYTES: u64 = 64;

/// Whether a node is a `cell` element.
fn is_cell_node(node: &Node) -> bool {
    matches!(node, Node::Element(element)
        if element.name.namespace.is_none() && element.name.name.as_str() == "cell")
}

/// Whether content is a single `cell` element, with spaces around it.
fn lone_cell(content: &Content) -> Option<usize> {
    let mut found = None;
    for (i, node) in content.nodes().iter().enumerate() {
        match node {
            Node::Space(_) => {}
            Node::Element(element)
                if found.is_none()
                    && element.name.namespace.is_none()
                    && element.name.name.as_str() == "cell" =>
            {
                found = Some(i);
            }
            _ => return None,
        }
    }
    found
}

impl Vm<'_> {
    /// Evaluates the content block of a grid call, its body: the one
    /// markup where pipe rows make cells. Returns the body, and the number
    /// of header rows a `|---|` line in it marks, with the line's span.
    pub(super) fn grid_body(
        &mut self,
        block: ast::ContentBlock<'_>,
    ) -> EvalResult<(Content, Option<(i64, Span)>)> {
        let header = separator(block.body())?;
        self.enter();
        self.rows = true;
        let line = std::mem::replace(&mut self.header_line, header.map(|(_, span)| span));
        let result = self.markup(block.body());
        self.header_line = line;
        self.rows = false;
        self.exit();
        Ok((result?, header))
    }

    /// The cells of a run of pipe rows on consecutive lines of a grid's
    /// body, placed as data rows are ([`Vm::data_cells`]): each line is a
    /// row of the grid, as wide as the run's widest, and a shorter one ends
    /// in empty cells. Separator lines make no row.
    pub(super) fn row_run(&mut self, rows: &[ast::TableRow<'_>]) -> EvalResult<Content> {
        let mut data = Vec::with_capacity(rows.len());
        for &row in rows {
            if self.header_line == Some(row.span()) {
                self.step(row.span())?;
                continue;
            }
            let cells = self.nest(|vm| vm.table_row(row))?;
            self.alloc(NODE_BYTES * cells.len() as u64, row.span())?;
            data.push((
                cells
                    .nodes()
                    .iter()
                    .map(|node| Value::Content(Content::node(node.clone())))
                    .collect(),
                row.span(),
            ));
            if self.flow_is_set() {
                break;
            }
        }
        self.data_cells(data)
    }

    /// The cells of a pipe row in a grid's body: each cell's markup in a
    /// `cell` element, or the `#cell(..)[..]` written in its place. A
    /// separator line makes none.
    pub(super) fn table_row(&mut self, row: ast::TableRow<'_>) -> EvalResult<Content> {
        self.step(row.span())?;
        let mut out = Content::new();
        let count = row.cells().count();
        for (i, cell) in row.cells().enumerate() {
            self.step(cell.span())?;
            let (mut content, mut label) = self.markup_labelled(cell)?;
            // A label at the end of a cell is the cell's, not that of the
            // element before it in the cell, unless that is a `#cell`.
            let trailing = cell
                .exprs()
                .rev()
                .find(|expr| !matches!(expr, ast::Expr::Space(_)))
                .and_then(|expr| match expr {
                    ast::Expr::Label(label) => Some(label),
                    _ => None,
                });
            if let Some(written) = trailing
                && label.is_none()
                && lone_cell(&content).is_none()
                && let Some(element) = content.last_element_mut()
                && element.label.as_deref() == Some(written.get())
            {
                element.label = None;
                label = Some(written);
            }
            // A label alone after the last pipe is the last cell's:
            // `| a | b | <t>`.
            if let Some(written) = label
                && i + 1 == count
                && i > 0
                && content
                    .nodes()
                    .iter()
                    .all(|node| matches!(node, Node::Space(_)))
                && !row
                    .to_untyped()
                    .children()
                    .last()
                    .is_some_and(|n| n.kind() == folio_syntax::SyntaxKind::TablePipe)
            {
                match out.last_element_mut() {
                    Some(last) if last.label.is_some() => {
                        self.tolerate(EvalError::new(
                            written.span(),
                            "this element already has a label",
                        ))?;
                    }
                    _ => out = super::vm::labelled(out, Some(written)),
                }
                break;
            }
            if content.nodes().iter().any(is_cell_node) && lone_cell(&content).is_none() {
                return Err(EvalError::new(
                    cell.span(),
                    "a cell of a pipe row that holds `#cell(..)[..]` holds nothing else",
                )
                .with_hint("separate the cells with `|`, or put the rest in the `#cell`'s body")
                .into());
            }
            let content = match lone_cell(&content) {
                Some(i) => Content::node(content.nodes()[i].clone()),
                None => self.builtin("cell", [("body", content.into())], cell.span())?,
            };
            out.extend(super::vm::labelled(content, label));
            // A `break`, `continue` or `return` ends the row.
            if self.flow_is_set() {
                break;
            }
        }
        Ok(out)
    }

    /// Turns the `rows` and `header` arguments of a grid into cells before
    /// the body's (SPEC §7.4): each row of the data is a row of the grid,
    /// with a cell for each value, and `header` adds a header row before
    /// them, or makes the data's first row the header row (`auto`). Each
    /// cell is given its place, so a row that is shorter than the others
    /// does not pull the next one up; the places a row leaves free get empty
    /// cells.
    pub(super) fn grid_rows(&mut self, args: Args, span: Span) -> EvalResult<Args> {
        let args_span = args.span;
        let named =
            |arg: &Arg, name: &str| arg.name.as_deref() == Some(name) && arg.scope.is_none();
        if !args
            .items
            .iter()
            .any(|arg| named(arg, "rows") || named(arg, "header") || named(arg, SEPARATOR))
        {
            return Ok(args);
        }
        // A `|---|` line's header rows, checked against all the arguments.
        let (separators, mut items): (Vec<Arg>, Vec<Arg>) = args
            .items
            .into_iter()
            .partition(|arg| named(arg, SEPARATOR));
        for separator in separators {
            if let Value::Int(rows) = separator.value {
                add_header_rows(&mut items, rows, separator.span)?;
            }
        }
        let mut rows_arg: Option<Arg> = None;
        let mut header_arg: Option<Arg> = None;
        let all = std::mem::take(&mut items);
        for arg in all {
            let slot = if named(&arg, "rows") {
                &mut rows_arg
            } else if named(&arg, "header") {
                &mut header_arg
            } else {
                items.push(arg);
                continue;
            };
            // Given twice, as `grid.with(..)` and the call may: as for any
            // other field.
            if slot.is_some() {
                bail!(
                    arg.span,
                    "duplicate field `{}`",
                    arg.name.as_deref().unwrap_or_default()
                );
            }
            *slot = Some(arg);
        }

        let mut data: Vec<(Vec<Value>, Span)> = Vec::new();
        if let Some(arg) = &header_arg {
            match &arg.value {
                Value::None | Value::Auto => {}
                value => {
                    let values = row_values(value).map_err(|message| {
                        EvalError::new(
                            arg.span,
                            eco_format!("`header` takes a row or `auto`: {message}"),
                        )
                    })?;
                    data.push((values, arg.span));
                }
            }
        }
        if let Some(arg) = &rows_arg {
            match &arg.value {
                Value::None => {}
                Value::Array(rows) => {
                    for (i, row) in rows.iter().enumerate() {
                        let values = row_values(row).map_err(|message| {
                            EvalError::new(
                                arg.span,
                                eco_format!("row {} of `rows`: {message}", i + 1),
                            )
                            .with_hint("each row is an array of cells: `(([a], [b]), ([c], [d]))`")
                        })?;
                        data.push((values, arg.span));
                    }
                }
                value => bail!(
                    arg.span,
                    "`rows` takes an array of rows, not {}",
                    a_type(value.type_name())
                ),
            }
        }

        // `header` makes one header row.
        if let Some(arg) = &header_arg
            && arg.value != Value::None
        {
            if arg.value == Value::Auto && data.is_empty() {
                bail!(
                    arg.span,
                    "`header: auto` makes the first row of `rows` the header row, but there is \
                     none"
                );
            }
            match items.iter().find(|item| named(item, HEADER_ROWS)) {
                Some(given) if given.value != Value::Int(1) => {
                    return Err(EvalError::new(
                        given.span,
                        eco_format!(
                            "`header` makes one header row, but `header-rows` is {}",
                            self.repr(&given.value, given.span)?
                        ),
                    )
                    .with_hint("leave out `header-rows`, or write the header as a row of `rows`")
                    .into());
                }
                Some(_) => {}
                None => items.push(Arg {
                    span: arg.span,
                    name: Some(HEADER_ROWS.into()),
                    scope: None,
                    value: Value::Int(1),
                    spelling: None,
                }),
            }
        }

        let cells = self.data_cells(data)?;
        if !cells.is_empty() {
            prepend_to_body(&mut items, cells, span);
        }
        Ok(Args::new(args_span, items.into_iter().collect()))
    }

    /// The cells of rows of data or of a run of pipe rows: each row starts
    /// a row of the grid, each cell at the first column that no cell above
    /// in the rows still covers, and places a row leaves free hold empty
    /// cells, up to the widest row. Each cell is given its column, so that
    /// auto-flow (SPEC §7.4) starts each row on a row of its own: its first
    /// cell's column is left of where the row before it ends, or that row
    /// is full there. No cell is given its row, which would bound the
    /// number of rows. A cell of the rows gives no place of its own, and
    /// spans no row past the last, since the places of other cells are
    /// known only within the rows.
    pub(super) fn data_cells(&mut self, data: Vec<(Vec<Value>, Span)>) -> EvalResult<Content> {
        let count = data.len();
        // Which places of each row are taken, and the cells of each row
        // with their columns.
        let mut taken: Vec<Vec<bool>> = vec![Vec::new(); count];
        let mut placed: Vec<(Vec<(usize, Content)>, Span)> = Vec::with_capacity(count);
        let mut columns = 0;
        for (r, (values, span)) in data.into_iter().enumerate() {
            placed.push((Vec::with_capacity(values.len()), span));
            let mut c = 0;
            for value in values {
                self.step(span)?;
                let mut content = self.data_cell(value, span)?;
                let Some(Node::Element(cell)) = content.nodes().first().cloned() else {
                    unreachable!("a data cell is a cell element");
                };
                let at = if cell.span.is_detached() {
                    span
                } else {
                    cell.span
                };
                if ["row", "column"]
                    .iter()
                    .any(|field| cell.field(field).is_some_and(|v| *v != Value::None))
                {
                    return Err(EvalError::new(
                        at,
                        "a cell of a row is placed by the row, not by `row` or `column`",
                    )
                    .with_hint("write a cell at a place of its own after the rows, as `#cell(row: .., column: ..)[..]`")
                    .into());
                }
                let span_of = |field: &str| match cell.field(field) {
                    Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(1).clamp(1, MAX_TRACKS),
                    _ => 1,
                };
                let (colspan, rowspan) = (span_of("colspan"), span_of("rowspan"));
                if r + rowspan > count {
                    return Err(EvalError::new(
                        at,
                        eco_format!(
                            "this cell spans {rowspan} rows, past the last of the rows it is in"
                        ),
                    )
                    .with_hint("a cell of a pipe row or of `rows` spans the rows written with it")
                    .into());
                }
                while taken[r].get(c).copied().unwrap_or(false) {
                    c += 1;
                }
                if c >= MAX_TRACKS {
                    bail!(span, "row {} has more than {MAX_TRACKS} columns", r + 1);
                }
                let end = (c + colspan).min(MAX_TRACKS);
                // The places a cell takes count against the memory limit.
                self.alloc(((end - c) * rowspan) as u64, span)?;
                for row in &mut taken[r..r + rowspan] {
                    if row.len() < end {
                        row.resize(end, false);
                    }
                    for place in &mut row[c..end] {
                        *place = true;
                    }
                }
                columns = columns.max(end);
                self.place_cell(&mut content, c)?;
                placed[r].0.push((c, content));
                c = end;
            }
        }
        // Empty cells where a row leaves places free.
        let mut out = Content::new();
        for (r, (mut cells, span)) in placed.into_iter().enumerate() {
            let row = taken[r].as_slice();
            for c in 0..columns {
                if !row.get(c).copied().unwrap_or(false) {
                    self.step(span)?;
                    let mut empty =
                        self.builtin("cell", [("body", Content::new().into())], span)?;
                    self.place_cell(&mut empty, c)?;
                    cells.push((c, empty));
                }
            }
            cells.sort_by_key(|(c, _)| *c);
            self.alloc(NODE_BYTES * cells.len() as u64, span)?;
            for (_, cell) in cells {
                out.extend(cell);
            }
        }
        Ok(out)
    }

    /// A cell of a data row: a `cell` element as it is, other content in a
    /// cell, and a string, a number or another value as its text.
    fn data_cell(&mut self, value: Value, span: Span) -> EvalResult<Content> {
        if let Value::Content(content) = &value {
            if let Some(i) = lone_cell(content) {
                return Ok(Content::node(content.nodes()[i].clone()));
            }
            if content.nodes().iter().any(is_cell_node) {
                return Err(EvalError::new(
                    span,
                    "a cell of `rows` that holds `cell(..)[..]` holds nothing else",
                )
                .with_hint("give each cell of the row as an item of its own")
                .into());
            }
        }
        let body = self.display(value, span)?;
        self.builtin("cell", [("body", body.into())], span)
    }

    /// Gives a cell of a row its column, counted from 1.
    fn place_cell(&mut self, content: &mut Content, column: usize) -> EvalResult<()> {
        let Some(cell) = content.last_element_mut() else {
            return Ok(());
        };
        let column = Value::Int(i64::try_from(column + 1).unwrap_or(i64::MAX));
        cell.fields.insert("column".into(), column);
        let name = cell.name.clone();
        if let Some(schema) = schema(self.db, self.project, &name) {
            let fields = std::mem::take(&mut cell.fields);
            cell.fields = canonical_fields(schema, fields);
        }
        Ok(())
    }
}

/// The values of one row of data.
fn row_values(value: &Value) -> Result<Vec<Value>, EcoString> {
    match value {
        Value::Array(cells) => Ok(cells.iter().cloned().collect()),
        value => Err(eco_format!(
            "a row is an array of cells, not {}",
            a_type(value.type_name())
        )),
    }
}

/// Puts cells before the body's content, or makes them the body.
fn prepend_to_body(items: &mut Vec<Arg>, cells: Content, span: Span) {
    let body = items
        .iter_mut()
        .find(|arg| arg.name.is_none() || arg.name.as_deref() == Some("body"));
    match body {
        Some(arg) => {
            let body = match &arg.value {
                Value::Content(content) => content.clone(),
                Value::Str(text) => Content::node(Node::Text {
                    text: text.clone(),
                    span: arg.span,
                }),
                Value::None => Content::new(),
                // Any other value is not a body, which instantiation
                // reports.
                _ => return,
            };
            let mut all = cells;
            all.extend(body);
            arg.value = Value::Content(all);
        }
        None => items.push(Arg {
            span,
            name: Some("body".into()),
            scope: None,
            value: Value::Content(cells),
            spelling: None,
        }),
    }
}

/// The header rows a `|---|` line in a grid's body marks: the pipe rows
/// above it, of which there must be at least one, and nothing else. A body
/// has at most one such line.
fn separator(body: ast::Markup<'_>) -> EvalResult<Option<(i64, Span)>> {
    let mut rows = 0i64;
    let mut other = false;
    let mut found: Option<(i64, Span)> = None;
    for item in body.items() {
        let ast::MarkupItem::Expr(expr) = item else {
            other = true;
            continue;
        };
        match expr {
            ast::Expr::Space(_) | ast::Expr::Parbreak(_) => {}
            // Only the first such line marks header rows; a later one is a
            // row of em dashes, `---` in markup.
            ast::Expr::TableRow(row) if row.is_separator() && found.is_none() => {
                if other {
                    return Err(EvalError::new(
                        row.span(),
                        "the rows above a `|---|` line must all be pipe rows",
                    )
                    .with_hint("write the header cells as a pipe row, or use `header-rows`")
                    .into());
                }
                if rows == 0 {
                    return Err(EvalError::new(
                        row.span(),
                        "a `|---|` line marks the rows above it as header rows, but there are none",
                    )
                    .with_hint("write the header row above it, or leave the line out")
                    .into());
                }
                found = Some((rows, row.span()));
            }
            ast::Expr::TableRow(_) => rows += 1,
            // Bindings and imports make no content.
            ast::Expr::LetBinding(_) | ast::Expr::ModuleImport(_) => {}
            _ => other = true,
        }
    }
    Ok(found)
}

/// The argument that carries the header rows a `|---|` line marks from a
/// grid's body to its instantiation, under a name no field has.
pub(super) fn separator_arg(rows: i64, span: Span) -> Arg {
    Arg {
        span,
        name: Some(SEPARATOR.into()),
        scope: None,
        value: Value::Int(rows),
        spelling: None,
    }
}

/// Adds the header rows a `|---|` line marks to a grid's arguments, unless
/// `header-rows` gives the same number. Another number is an error, as is
/// the line in a grid with `rows` or `header`, whose header row comes
/// before the body's rows.
fn add_header_rows(items: &mut Vec<Arg>, rows: i64, span: Span) -> EvalResult<()> {
    let named = |arg: &Arg, name: &str| arg.name.as_deref() == Some(name) && arg.scope.is_none();
    if items
        .iter()
        .any(|arg| (named(arg, "rows") || named(arg, "header")) && arg.value != Value::None)
    {
        return Err(EvalError::new(
            span,
            "a `|---|` line cannot mark header rows in a grid with `rows` or `header`, whose \
             rows come before the body's",
        )
        .with_hint("leave out the line, or give the header with `header`")
        .into());
    }
    match items.iter().find(|arg| named(arg, HEADER_ROWS)) {
        Some(arg) if arg.value == Value::Int(rows) => Ok(()),
        Some(arg) => Err(EvalError::new(
            span,
            eco_format!(
                "this line marks {rows} header row{}, but `header-rows` is {}",
                if rows == 1 { "" } else { "s" },
                match &arg.value {
                    Value::Int(n) => eco_format!("{n}"),
                    other => eco_format!("{}", a_type(other.type_name())),
                }
            ),
        )
        .with_hint("leave out `header-rows`, or move the line")
        .into()),
        None => {
            items.push(Arg {
                span,
                name: Some(HEADER_ROWS.into()),
                scope: None,
                value: Value::Int(rows),
                spelling: None,
            });
            Ok(())
        }
    }
}

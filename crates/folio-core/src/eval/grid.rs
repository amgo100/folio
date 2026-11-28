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
        let result = self.markup(block.body());
        self.rows = false;
        self.exit();
        Ok((result?, header))
    }

    /// The cells of a pipe row in a grid's body: each cell's markup in a
    /// `cell` element, or the `#cell(..)[..]` written in its place. A
    /// separator line makes none.
    pub(super) fn table_row(&mut self, row: ast::TableRow<'_>) -> EvalResult<Content> {
        self.step(row.span())?;
        let mut out = Content::new();
        if row.is_separator() {
            return Ok(out);
        }
        for cell in row.cells() {
            self.step(cell.span())?;
            let (content, label) = self.markup_labelled(cell)?;
            let content = match lone_cell(&content) {
                Some(i) => Content::node(content.nodes()[i].clone()),
                None => self.builtin("cell", [("body", content.into())], cell.span())?,
            };
            out.extend(super::vm::labelled(content, label));
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
        let mut rows_arg = None;
        let mut header_arg = None;
        let all = std::mem::take(&mut items);
        for arg in all {
            if named(&arg, "rows") {
                rows_arg = Some(arg);
            } else if named(&arg, "header") {
                header_arg = Some(arg);
            } else {
                items.push(arg);
            }
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

    /// The cells of data rows, each at its place: rows in order from the
    /// grid's first row, cells from the first column that no cell above
    /// still covers.
    fn data_cells(&mut self, data: Vec<(Vec<Value>, Span)>) -> EvalResult<Content> {
        // Which places of each row are taken, and the cells of each row
        // with their columns.
        let mut taken: Vec<Vec<bool>> = Vec::new();
        let mut placed: Vec<(Vec<(usize, Content)>, Span)> = Vec::new();
        let mut columns = 0;
        // Cells made first, so that the places of those placed by their own
        // fields are known before the others are placed.
        let mut made: Vec<(Vec<Content>, Span)> = Vec::with_capacity(data.len());
        for (values, span) in data {
            let mut cells = Vec::with_capacity(values.len());
            for value in values {
                self.step(span)?;
                let content = self.data_cell(value, span)?;
                if let Some(Node::Element(cell)) = content.nodes().first() {
                    let given = |field: &str| match cell.field(field) {
                        Some(Value::Int(n)) => {
                            Some(usize::try_from(*n).unwrap_or(1).clamp(1, MAX_TRACKS))
                        }
                        _ => None,
                    };
                    match (given("row"), given("column")) {
                        (Some(row), Some(column)) => {
                            let spans = |field: &str| given(field).unwrap_or(1);
                            let (r, c) = (row - 1, column - 1);
                            let end = (c + spans("colspan")).min(MAX_TRACKS);
                            let rowspan = spans("rowspan");
                            self.alloc(((end - c) * rowspan) as u64, span)?;
                            if taken.len() < r + rowspan {
                                taken.resize(r + rowspan, Vec::new());
                            }
                            for row in &mut taken[r..r + rowspan] {
                                if row.len() < end {
                                    row.resize(end, false);
                                }
                                for place in &mut row[c..end] {
                                    *place = true;
                                }
                            }
                            columns = columns.max(end);
                        }
                        (None, None) => {}
                        _ => bail!(
                            span,
                            "a cell of `rows` gives its place by both `row` and `column`, or by \
                             neither"
                        ),
                    }
                }
                cells.push(content);
            }
            made.push((cells, span));
        }
        for (r, (values, span)) in made.into_iter().enumerate() {
            if taken.len() <= r {
                taken.resize(r + 1, Vec::new());
            }
            placed.push((Vec::with_capacity(values.len()), span));
            let mut c = 0;
            for mut content in values {
                let Some(Node::Element(cell)) = content.nodes().first().cloned() else {
                    unreachable!("a data cell is a cell element");
                };
                // A cell placed by its own fields keeps its place.
                if cell.field("column").is_some_and(|v| *v != Value::None)
                    || cell.field("row").is_some_and(|v| *v != Value::None)
                {
                    placed[r].0.push((usize::MAX, content));
                    continue;
                }
                let span_of = |field: &str| match cell.field(field) {
                    Some(Value::Int(n)) => usize::try_from(*n).unwrap_or(1).clamp(1, MAX_TRACKS),
                    _ => 1,
                };
                let (colspan, rowspan) = (span_of("colspan"), span_of("rowspan"));
                while taken[r].get(c).copied().unwrap_or(false) {
                    c += 1;
                }
                if c >= MAX_TRACKS {
                    bail!(span, "row {} has more than {MAX_TRACKS} columns", r + 1);
                }
                let end = (c + colspan).min(MAX_TRACKS);
                // The places a cell takes count against the memory limit.
                self.alloc(((end - c) * rowspan) as u64, span)?;
                if taken.len() < r + rowspan {
                    taken.resize(r + rowspan, Vec::new());
                }
                for row in &mut taken[r..r + rowspan] {
                    if row.len() < end {
                        row.resize(end, false);
                    }
                    for place in &mut row[c..end] {
                        *place = true;
                    }
                }
                columns = columns.max(end);
                self.place_cell(&mut content, c, r)?;
                placed[r].0.push((c, content));
                c = end;
            }
        }
        // Empty cells where a row leaves places free.
        let mut out = Content::new();
        for (r, (mut cells, span)) in placed.into_iter().enumerate() {
            let row = taken.get(r).map(Vec::as_slice).unwrap_or_default();
            for c in 0..columns {
                if !row.get(c).copied().unwrap_or(false) {
                    self.step(span)?;
                    let mut empty =
                        self.builtin("cell", [("body", Content::new().into())], span)?;
                    self.place_cell(&mut empty, c, r)?;
                    cells.push((c, empty));
                }
            }
            cells.sort_by_key(|(c, _)| *c);
            self.alloc(64 * cells.len() as u64, span)?;
            for (_, cell) in cells {
                out.extend(cell);
            }
        }
        Ok(out)
    }

    /// A cell of a data row: a `cell` element as it is, other content in a
    /// cell, and a string, a number or another value as its text.
    fn data_cell(&mut self, value: Value, span: Span) -> EvalResult<Content> {
        if let Value::Content(content) = &value
            && let Some(i) = lone_cell(content)
        {
            return Ok(Content::node(content.nodes()[i].clone()));
        }
        let body = self.display(value, span)?;
        self.builtin("cell", [("body", body.into())], span)
    }

    /// Gives a data cell its column and row, counted from 1.
    fn place_cell(&mut self, content: &mut Content, column: usize, row: usize) -> EvalResult<()> {
        let Some(cell) = content.last_element_mut() else {
            return Ok(());
        };
        let to_int = |n: usize| Value::Int(i64::try_from(n + 1).unwrap_or(i64::MAX));
        cell.fields.insert("column".into(), to_int(column));
        cell.fields.insert("row".into(), to_int(row));
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
            ast::Expr::TableRow(row) if row.is_separator() => {
                if found.is_some() {
                    return Err(EvalError::new(
                        row.span(),
                        "a grid's body has one `|---|` line at most",
                    )
                    .with_hint("the rows above the first one are the header rows")
                    .into());
                }
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

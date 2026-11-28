//! The exam's render templates (SPEC §5.3, §13.1), written in the
//! stylesheet language from the closed primitive set.

use ecow::EcoVec;
use folio_component::TemplateDecl;

use crate::elements::{ANSWER, MARK, PART, QUESTION};
use crate::name;

/// The name of the template that sets an answer in a framed box.
pub const MARKSCHEME_BOX: &str = "markscheme-box";
/// The name of the template that sets a mark code as a badge.
pub const BADGE: &str = "badge";
/// The name of the template that sets a question or part as a row of the
/// mark scheme's table: its number, what the variant shows of its body
/// (its answers and notes, with `show-only`), and its marks.
pub const MARKSCHEME_TABLE: &str = "markscheme-table";

/// A question: its number beside its body, and after it the trailer with
/// its marks, when it gives them.
const QUESTION_DEFAULT: &str = "
    body: stack(grid(strong(counter()), slot(), columns: 8mm 1fr))
          box(strong(\"(Total for \" term() \" \" counter() \": \" field(marks) \" \"
              if field(marks) == 1 { \"mark\" } else { \"marks\" } \")\")) if field(marks);
";

/// A part: its number beside its body, and its marks in brackets at the
/// end of the row.
const PART_DEFAULT: &str = "
    body: stack(grid(counter(), slot(), align-end(\"[\" field(marks) \"]\") if field(marks),
                     columns: 9mm 1fr auto));
";

/// A row of the mark scheme's table: the number, what the variant shows
/// of the body (its answers and notes, with `show-only`), and the marks.
const TABLE_ROW: &str = "
    body: stack(stack(grid(strong(counter()), slot(), align-end(field(marks)) if field(marks),
                           columns: 18mm 1fr 15mm)));
";

// Each template wraps its grid in a `stack`, and the table's in two, so
// that the default stylesheet's rules (`question > stack > grid`,
// `question > stack > stack > grid`) style the grid the template makes
// and never a table the question holds: `grid` is also a built-in
// element, and `stack` is only a template's part.

/// The component's templates.
pub fn templates() -> EcoVec<TemplateDecl> {
    EcoVec::from([
        TemplateDecl::new(name(QUESTION), QUESTION_DEFAULT),
        TemplateDecl::new(name(PART), PART_DEFAULT),
        TemplateDecl::named(name(QUESTION), name(MARKSCHEME_TABLE), TABLE_ROW),
        TemplateDecl::named(name(PART), name(MARKSCHEME_TABLE), TABLE_ROW),
        TemplateDecl::named(name(ANSWER), name(MARKSCHEME_BOX), "body: box(slot());"),
        TemplateDecl::named(name(MARK), name(BADGE), "body: badge(field(code));"),
    ])
}

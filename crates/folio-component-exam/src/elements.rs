//! The exam's elements (SPEC §13.1) and the words they generate.

use ecow::EcoVec;
use folio_component::{
    Body, ContentKind, CounterDecl, ElementKind, ElementSchema, FieldDecl, FieldType, LocaleString,
    QualifiedName, Value,
};

use crate::name;

/// A question: `#question(marks: 3)[..]`. It counts through the paper.
pub const QUESTION: &str = "question";
/// A part of a question or of another part: `#part(marks: 2)[..]`. Parts
/// are counted within their question or part, so they nest.
pub const PART: &str = "part";
/// An answer, which the exam paper shows as answer space and the mark
/// scheme shows: `#answer(lines: 4)[..]`.
pub const ANSWER: &str = "answer";
/// A mark code in an answer: `#mark("M1")`.
pub const MARK: &str = "mark";
/// A note for the marker, which only the mark scheme shows.
pub const MARKSCHEME_NOTE: &str = "markscheme-note";

/// The field of questions and parts that holds their marks.
pub const MARKS: &str = "marks";
/// The field of answers that holds their number of ruled lines.
pub const LINES: &str = "lines";
/// The field of mark codes that holds the code.
pub const CODE: &str = "code";

/// The schemas of the exam's elements.
pub fn schemas() -> EcoVec<ElementSchema> {
    let marks = || FieldDecl::with_default(name(MARKS), FieldType::Number, Value::None);

    let mut question = ElementSchema::new(name(QUESTION), ElementKind::Block);
    question.fields = EcoVec::from([marks()]);
    question.body = Body::Content(ContentKind::Blocks);
    question.counter = Some(CounterDecl::new());
    question.ref_key = Some(name(QUESTION));

    let mut part = ElementSchema::new(name(PART), ElementKind::Block);
    part.fields = EcoVec::from([marks()]);
    part.body = Body::Content(ContentKind::Blocks);
    let mut counter = CounterDecl::new();
    counter.scope = EcoVec::from([
        QualifiedName::unqualified(name(QUESTION)),
        QualifiedName::unqualified(name(PART)),
    ]);
    part.counter = Some(counter);
    part.ref_key = Some(name(PART));

    let mut answer = ElementSchema::new(name(ANSWER), ElementKind::Block);
    answer.fields = EcoVec::from([FieldDecl::with_default(
        name(LINES),
        FieldType::Int,
        Value::None,
    )]);
    answer.body = Body::Content(ContentKind::Blocks);

    let mut mark = ElementSchema::new(name(MARK), ElementKind::Inline);
    mark.fields = EcoVec::from([FieldDecl::required(name(CODE), FieldType::Str)]);
    mark.positional = EcoVec::from([name(CODE)]);

    let mut note = ElementSchema::new(name(MARKSCHEME_NOTE), ElementKind::Block);
    note.body = Body::Content(ContentKind::Blocks);

    EcoVec::from([question, part, answer, mark, note])
}

/// The words of the exam's reference keys (SPEC §4.5), in English and
/// Polish. The Polish words are abbreviations that do not inflect, as the
/// core's are, so a reference reads right in any case of its sentence.
pub fn locale_strings() -> EcoVec<LocaleString> {
    EcoVec::from([
        LocaleString::new("en", name(QUESTION), "Question"),
        LocaleString::new("pl", name(QUESTION), "zad."),
        LocaleString::new("en", name(PART), "part"),
        LocaleString::new("pl", name(PART), "cz."),
    ])
}

//! The Templates extension point: default render templates for a
//! component's elements (SPEC §3.2, §5.3).

use ecow::EcoString;

use crate::names::Name;

/// A render template a component provides for one of its elements.
///
/// The template is source text in the stylesheet language, so that it is
/// built from the closed primitive set of SPEC §5.3 and has a defined export
/// to Office like any template a stylesheet writes. The core parses and
/// checks it when the component is loaded.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TemplateDecl {
    /// The element the template renders, within the component's namespace.
    pub element: Name,
    /// The template's name, or `None` for the element's default template.
    /// Stylesheets select a named template with `template: <name>`.
    pub name: Option<Name>,
    /// The template's parts: the declarations between the braces of a
    /// `render` block, such as `body: slot(statement) rule(0.4pt);`.
    pub parts: EcoString,
}

impl TemplateDecl {
    /// The default template of an element.
    pub fn new(element: Name, parts: impl Into<EcoString>) -> Self {
        Self {
            element,
            name: None,
            parts: parts.into(),
        }
    }

    /// A named template of an element, which stylesheets select with
    /// `template: <name>`.
    pub fn named(element: Name, name: Name, parts: impl Into<EcoString>) -> Self {
        Self {
            element,
            name: Some(name),
            parts: parts.into(),
        }
    }
}

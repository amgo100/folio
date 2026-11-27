//! The variables a closure captures: the free variables of its body.
//!
//! A closure holds only these, not its whole environment, so it changes only
//! when one of them does, and memoised calls of it stay valid when anything
//! else in its file changes (SPEC §2.3).

use std::collections::{BTreeMap, BTreeSet};

use ecow::EcoString;
use folio_syntax::ast::{self, AstNode};
use folio_syntax::{SyntaxKind, SyntaxNode};

use super::value::Value;

/// Looks up the free variables of a closure ([`free_variables`]) that the
/// defining scope has with `lookup`. Variables it does not have are
/// globals, or are defined at call time (the closure's own name), or are
/// errors that the call reports.
pub(crate) fn captures(
    free: BTreeSet<EcoString>,
    lookup: impl Fn(&str) -> Option<Value>,
) -> BTreeMap<EcoString, Value> {
    free.into_iter()
        .filter_map(|name| lookup(&name).map(|value| (name, value)))
        .collect()
}

/// The variables a closure's body and defaults use but do not bind.
pub(crate) fn free_variables(closure: ast::Closure<'_>) -> BTreeSet<EcoString> {
    let mut visitor = Visitor {
        bound: vec![BTreeSet::new()],
        free: BTreeSet::new(),
    };
    visitor.closure(closure);
    visitor.free
}

struct Visitor {
    /// The names bound inside the closure, by scope.
    bound: Vec<BTreeSet<EcoString>>,
    /// The names used but not bound inside.
    free: BTreeSet<EcoString>,
}

impl Visitor {
    fn bind(&mut self, ident: ast::Ident<'_>) {
        self.bound
            .last_mut()
            .expect("there is always a scope")
            .insert(ident.get().clone());
    }

    fn bind_pattern(&mut self, pattern: ast::Pattern<'_>) {
        for ident in pattern.bindings() {
            self.bind(ident);
        }
    }

    fn reference(&mut self, ident: ast::Ident<'_>) {
        let name = ident.get();
        if !self.bound.iter().any(|scope| scope.contains(name)) {
            self.free.insert(name.clone());
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.bound.push(BTreeSet::new());
        f(self);
        self.bound.pop();
    }

    fn closure(&mut self, closure: ast::Closure<'_>) {
        // Defaults are evaluated where the closure is made.
        for param in closure.params().children() {
            if let ast::Param::Named(named) = param {
                self.visit(named.expr().to_untyped());
            }
        }
        self.scoped(|v| {
            if let Some(name) = closure.name() {
                v.bind(name);
            }
            for param in closure.params().children() {
                match param {
                    ast::Param::Pos(pattern) => v.bind_pattern(pattern),
                    ast::Param::Named(named) => v.bind(named.name()),
                    ast::Param::Spread(spread) => {
                        if let Some(ident) = spread.sink_ident() {
                            v.bind(ident);
                        }
                    }
                }
            }
            v.visit(closure.body().to_untyped());
        });
    }

    fn visit(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::Ident => {
                if let Some(ident) = node.cast() {
                    self.reference(ident);
                }
            }
            SyntaxKind::CodeBlock | SyntaxKind::ContentBlock => {
                self.scoped(|v| v.children(node));
            }
            // The field of a field access is not a variable.
            SyntaxKind::FieldAccess => {
                if let Some(access) = node.cast::<ast::FieldAccess<'_>>() {
                    self.visit(access.target().to_untyped());
                }
            }
            // Nor is the name of a named argument or pair.
            SyntaxKind::Named => {
                if let Some(named) = node.cast::<ast::Named<'_>>() {
                    self.visit(named.expr().to_untyped());
                }
            }
            SyntaxKind::Closure => {
                if let Some(closure) = node.cast() {
                    self.closure(closure);
                }
            }
            SyntaxKind::LetBinding => {
                if let Some(binding) = node.cast::<ast::LetBinding<'_>>() {
                    if let Some(init) = binding.init() {
                        self.visit(init.to_untyped());
                    }
                    for ident in binding.kind().bindings() {
                        self.bind(ident);
                    }
                }
            }
            SyntaxKind::ForLoop => {
                if let Some(for_loop) = node.cast::<ast::ForLoop<'_>>() {
                    self.visit(for_loop.iterable().to_untyped());
                    self.scoped(|v| {
                        v.bind_pattern(for_loop.pattern());
                        v.visit(for_loop.body().to_untyped());
                    });
                }
            }
            SyntaxKind::ModuleImport => {
                if let Some(import) = node.cast::<ast::ModuleImport<'_>>() {
                    self.visit(import.source().to_untyped());
                    if let Some(ast::Imports::Items(items)) = import.imports() {
                        for item in items.iter() {
                            self.bind(item.bound_name());
                        }
                    }
                    if let Some(name) = import.new_name() {
                        self.bind(name);
                    } else if import.imports().is_none()
                        && let Ok(name) = import.bare_name()
                    {
                        self.bound
                            .last_mut()
                            .expect("there is always a scope")
                            .insert(name);
                    }
                }
            }
            // A name in an equation reads the script's bindings before the
            // names of math. A dotted name is a symbol.
            SyntaxKind::MathIdent => {
                let name = node.leaf_text();
                if !self.bound.iter().any(|scope| scope.contains(name)) {
                    self.free.insert(name.clone());
                }
            }
            SyntaxKind::MathFieldAccess => {}
            // LaTeX math reads no bindings, and rules and element
            // declarations are not evaluated by scripts.
            SyntaxKind::TexEquation | SyntaxKind::ScopedRule | SyntaxKind::ElementDecl => {}
            _ => self.children(node),
        }
    }

    /// Visits a node's children, on a new stack segment if the stack is
    /// running low: a chain of a math operator such as `1/1/…/x` nests the
    /// syntax as deeply as it is long.
    fn children(&mut self, node: &SyntaxNode) {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
            for child in node.children() {
                self.visit(child);
            }
        });
    }
}

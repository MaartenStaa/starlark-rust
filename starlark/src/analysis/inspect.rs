use crate::codemap::CodeMap;
use crate::codemap::Pos;
use crate::codemap::ResolvedSpan;
use crate::codemap::Span;
use crate::syntax::ast::ArgumentP;
use crate::syntax::ast::AstExprP;
use crate::syntax::ast::AstLiteral;
use crate::syntax::ast::AstNoPayload;
use crate::syntax::ast::AstStmtP;
use crate::syntax::ast::ExprP;
use crate::syntax::ast::ParameterP;
use crate::syntax::ast::StmtP;
use crate::syntax::uniplate::Visit;
use crate::syntax::AstModule;

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum AutocompleteType {
    /// Offer completions of all available symbol. Cursor is e.g. at the start of a line,
    /// or in the right hand side of an assignment.
    Default,
    /// Offer completions of loadable modules. Cursor is in the module path of a load statement.
    LoadPath {
        current_value: String,
        current_span: ResolvedSpan,
    },
    /// Offer completions of symbols in a loaded module. Cursor is in a load statement, but
    /// after the module path.
    LoadSymbol {
        path: String,
        current_span: ResolvedSpan,
        previously_loaded: Vec<String>,
    },
    /// Offer completions of target names. Cursor is in a literal string, but not in a load statement
    /// or a visibility-like declaration.
    String {
        current_value: String,
        current_span: ResolvedSpan,
    },
    /// Offer completions of function parameters names. Cursor is in a function call. ALSO offer
    /// regular symbol completions, since this might be e.g. a positional argument, in which cases
    /// parameter names don't matter/help the user.
    Parameter {
        function_name: String,
        function_name_span: ResolvedSpan,
    },
    /// Offer completions of type names.
    Type,
    /// Don't offer any completions. Cursor is e.g. in a comment.
    None,
}

impl AstModule {
    /// Walks through the AST to find the type of the expression at the given position.
    /// Based on that, returns an enum that can be used to determine what kind of
    /// autocomplete should be performed. For example, path in a `load` statement versus
    /// a variable name.
    pub fn get_auto_complete_type(&self, line: u32, col: u32) -> Option<AutocompleteType> {
        let line_span = match self.codemap.line_span_opt(line as usize) {
            None => {
                // The document got edited to add new lines, just bail out
                return None;
            }
            Some(line_span) => line_span,
        };
        let current_pos = std::cmp::min(line_span.begin() + col, line_span.end());

        // Walk through the AST to find a node matching the current position.
        fn walk_and_find_completion_type(
            codemap: &CodeMap,
            position: Pos,
            stmt: Visit<AstNoPayload>,
        ) -> Option<AutocompleteType> {
            // Utility function to get the span of a string literal without the quotes.
            fn string_span_without_quotes(codemap: &CodeMap, span: Span) -> ResolvedSpan {
                let mut span = codemap.resolve_span(span);
                span.begin_column += 1;
                span.end_column -= 1;
                span
            }

            let span = match &stmt {
                Visit::Stmt(stmt) => stmt.span,
                Visit::Expr(expr) => expr.span,
            };
            let contains_pos = span.contains(position);
            if !contains_pos {
                return None;
            }

            match &stmt {
                Visit::Stmt(AstStmtP {
                    node: StmtP::Assign(dest, rhs),
                    ..
                }) => {
                    if dest.span.contains(position) {
                        return Some(AutocompleteType::None);
                    }
                    let (type_, expr) = &**rhs;
                    if let Some(type_) = type_ {
                        if type_.span.contains(position) {
                            return Some(AutocompleteType::Type);
                        }
                    }
                    if expr.span.contains(position) {
                        return walk_and_find_completion_type(codemap, position, Visit::Expr(expr));
                    }
                }
                Visit::Stmt(AstStmtP {
                    node: StmtP::AssignModify(dest, _, expr),
                    ..
                }) => {
                    if dest.span.contains(position) {
                        return Some(AutocompleteType::None);
                    } else if expr.span.contains(position) {
                        return walk_and_find_completion_type(codemap, position, Visit::Expr(expr));
                    }
                }
                Visit::Stmt(AstStmtP {
                    node: StmtP::Load(load),
                    ..
                }) => {
                    if load.module.span.contains(position) {
                        return Some(AutocompleteType::LoadPath {
                            current_value: load.module.to_string(),
                            current_span: string_span_without_quotes(codemap, load.module.span),
                        });
                    }

                    for (name, _) in &load.args {
                        if name.span.contains(position) {
                            return Some(AutocompleteType::LoadSymbol {
                                path: load.module.to_string(),
                                current_span: string_span_without_quotes(codemap, name.span),
                                previously_loaded: load
                                    .args
                                    .iter()
                                    .filter(|(n, _)| n != name)
                                    .map(|(n, _)| n.to_string())
                                    .collect(),
                            });
                        }
                    }

                    return Some(AutocompleteType::None);
                }
                Visit::Stmt(AstStmtP {
                    node: StmtP::Def(def),
                    ..
                }) => {
                    // If the cursor is in the name of the function, don't offer any completions.
                    if def.name.span.contains(position) {
                        return Some(AutocompleteType::None);
                    }
                    // If the cursor is in one of the arguments, only offer completions for
                    // default values for the arguments.
                    for arg in def.params.iter() {
                        if !arg.span.contains(position) {
                            continue;
                        }
                        match &arg.node {
                            ParameterP::Normal(_, type_) => {
                                if let Some(type_) = type_ {
                                    if type_.span.contains(position) {
                                        return Some(AutocompleteType::Type);
                                    }
                                }
                            }
                            ParameterP::WithDefaultValue(_, type_, expr) => {
                                if let Some(type_) = type_ {
                                    if type_.span.contains(position) {
                                        return Some(AutocompleteType::Type);
                                    }
                                }
                                if expr.span.contains(position) {
                                    return walk_and_find_completion_type(
                                        codemap,
                                        position,
                                        Visit::Expr(expr),
                                    );
                                }
                            }
                            _ => {}
                        }

                        return Some(AutocompleteType::None);
                    }
                    if let Some(return_type) = &def.return_type {
                        if return_type.span.contains(position) {
                            return Some(AutocompleteType::Type);
                        }
                    }

                    return walk_and_find_completion_type(
                        codemap,
                        position,
                        Visit::Stmt(&def.body),
                    );
                }
                Visit::Expr(AstExprP {
                    node: ExprP::Call(name, args),
                    span,
                }) => {
                    if name.span.contains(position) {
                        return Some(AutocompleteType::Default);
                    }
                    for arg in args {
                        if !arg.span.contains(position) {
                            continue;
                        }
                        match &arg.node {
                            ArgumentP::Named(arg_name, value) => {
                                if arg_name.span.contains(position) {
                                    return Some(AutocompleteType::Parameter {
                                        function_name: name.to_string(),
                                        function_name_span: codemap.resolve_span(name.span),
                                    });
                                } else if value.span.contains(position) {
                                    return walk_and_find_completion_type(
                                        codemap,
                                        position,
                                        Visit::Expr(value),
                                    );
                                }
                            }
                            ArgumentP::Positional(expr) => {
                                return match expr {
                                    AstExprP {
                                        node: ExprP::Identifier(_),
                                        ..
                                    } => {
                                        // Typing a literal, might be meant as a parameter name.
                                        Some(AutocompleteType::Parameter {
                                            function_name: name.to_string(),
                                            function_name_span: codemap.resolve_span(name.span),
                                        })
                                    }
                                    _ => walk_and_find_completion_type(
                                        codemap,
                                        position,
                                        Visit::Expr(expr),
                                    ),
                                };
                            }
                            ArgumentP::Args(expr) | ArgumentP::KwArgs(expr) => {
                                return walk_and_find_completion_type(
                                    codemap,
                                    position,
                                    Visit::Expr(expr),
                                );
                            }
                        }
                    }
                    // No matches? We might be in between empty braces (new function call),
                    // e.g. `foo(|)`. However, we don't want to offer completions for
                    // when the cursor is at the very end of the function call, e.g. `foo()|`.
                    return Some(if args.is_empty() && span.end() != position {
                        AutocompleteType::Parameter {
                            function_name: name.to_string(),
                            function_name_span: codemap.resolve_span(name.span),
                        }
                    } else if !args.is_empty() {
                        AutocompleteType::Default
                    } else {
                        // Don't offer completions right after the function call.
                        AutocompleteType::None
                    });
                }
                Visit::Expr(AstExprP {
                    node: ExprP::Literal(AstLiteral::String(str)),
                    ..
                }) => {
                    return Some(AutocompleteType::String {
                        current_value: str.to_string(),
                        current_span: string_span_without_quotes(codemap, span),
                    });
                }
                Visit::Stmt(stmt) => {
                    let mut result = None;
                    stmt.visit_children(|stmt| {
                        if let Some(r) = walk_and_find_completion_type(codemap, position, stmt) {
                            result = Some(r);
                        }
                    });
                    return result;
                }
                Visit::Expr(expr) => {
                    let mut result = None;
                    expr.visit_expr(|expr| {
                        if let Some(r) =
                            walk_and_find_completion_type(codemap, position, Visit::Expr(expr))
                        {
                            result = Some(r);
                        }
                    });
                    return result;
                }
            }

            None
        }

        walk_and_find_completion_type(&self.codemap, current_pos, Visit::Stmt(&self.statement))
            .or(Some(AutocompleteType::Default))
    }
}

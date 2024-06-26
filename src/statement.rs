use tower_lsp::lsp_types::Range;

use crate::expression::Expression;
use crate::token::{Error, Result, Token, TokenIter, TokenType};

use std::fmt;

pub struct Body {
    pub left_brace: Token,
    pub statements: Vec<Statement>,
    pub right_brace: Token,
}

// Let statements introduce new named constants. For example:
//   let a: int = x + 2
// The name token can either be an identifier or a number.
pub struct LetStatement {
    pub name: String,
    pub name_token: Token,
    pub type_expr: Expression,
    pub value: Expression,
}

// Define statements introduce new named functions. For example:
//   define foo(a: int, b: int) -> int = a + a + b
pub struct DefineStatement {
    pub name: String,
    pub name_token: Token,

    // For templated definitions
    pub type_params: Vec<Token>,

    // A list of the named arg types, like "a: int" and "b: int".
    pub args: Vec<Expression>,

    // The specified return type of the function, like "int"
    pub return_type: Expression,

    // The body of the function, like "a + a + b"
    pub return_value: Expression,
}

// There are two keywords for theorems.
// The "axiom" keyword indicates theorems that are axiomatic.
// The "theorem" keyword is used for the vast majority of normal theorems.
// For example, in:
//   axiom foo(p, q): p -> (q -> p)
// axiomatic would be "true", the name is "foo", the args are p, q, and the claim is "p -> (q -> p)".
pub struct TheoremStatement {
    pub axiomatic: bool,
    pub name: String,
    pub type_params: Vec<Token>,
    pub args: Vec<Expression>,
    pub claim: Expression,
    pub body: Option<Body>,
}

// Prop statements are a boolean expression.
// We're implicitly asserting that it is true and provable.
// It's like an anonymous theorem.
pub struct PropStatement {
    pub claim: Expression,
}

// Type statements associate a name with a type expression
pub struct TypeStatement {
    pub name: String,
    pub type_expr: Expression,
}

// ForAll statements create a new block in which new variables are introduced.
pub struct ForAllStatement {
    pub quantifiers: Vec<Expression>,
    pub body: Body,
}

// If statements create a new block that introduces no variables but has an implicit condition.
// They can optionally create a second block with an "else" keyword followed by a block.
pub struct IfStatement {
    pub condition: Expression,
    pub body: Body,
    pub else_body: Option<Body>,

    // Just for error reporting
    pub token: Token,
}

// Exists statements introduce new variables to the outside block.
pub struct ExistsStatement {
    pub quantifiers: Vec<Expression>,
    pub claim: Expression,
}

// Struct statements define a new type
pub struct StructStatement {
    pub name: String,
    pub name_token: Token,

    // Each field contains a field name-token and a type expression
    pub fields: Vec<(Token, Expression)>,
}

pub struct ImportStatement {
    // The full path to the module, like in "foo.bar.baz" the module would be ["foo", "bar", "baz"]
    pub components: Vec<String>,

    // What names to import from the module.
    // If this is empty, we just import the module itself.
    pub names: Vec<Token>,
}

// A class statement defines some class variables and instance methods that are scoped to the class.
pub struct ClassStatement {
    pub name: String,
    pub name_token: Token,

    // The body of a class statement
    pub body: Body,
}

// A default statement determines what class is used for numeric literals.
pub struct DefaultStatement {
    pub type_expr: Expression,
}

pub struct SolveStatement {
    // The expression we are trying to find equalities for.
    pub target: Expression,

    // Statements used to solve the problem.
    pub body: Body,
}

// Acorn is a statement-based language. There are several types.
// Each type has its own struct.
pub struct Statement {
    pub first_token: Token,
    pub last_token: Token,
    pub statement: StatementInfo,
}

// Information about a statement that is specific to the type of statement it is
pub enum StatementInfo {
    Let(LetStatement),
    Define(DefineStatement),
    Theorem(TheoremStatement),
    Prop(PropStatement),
    Type(TypeStatement),
    ForAll(ForAllStatement),
    If(IfStatement),
    Exists(ExistsStatement),
    Struct(StructStatement),
    Import(ImportStatement),
    Class(ClassStatement),
    Default(DefaultStatement),
    Solve(SolveStatement),
}

const ONE_INDENT: &str = "    ";

fn add_indent(indentation: &str) -> String {
    format!("{}{}", indentation, ONE_INDENT)
}

// Writes out a block, starting with the space before the open-brace, indenting the rest.
// Does not write a trailing newline.
fn write_block(f: &mut fmt::Formatter, statements: &[Statement], indentation: &str) -> fmt::Result {
    write!(f, " {{\n")?;
    let new_indentation = add_indent(indentation);
    for s in statements {
        s.fmt_helper(f, &new_indentation)?;
        write!(f, "\n")?;
    }
    write!(f, "{}}}", indentation)
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_helper(f, "")
    }
}

// Parses a block (a list of statements) where the left brace has already been consumed.
// Returns the statements along with the token for the final right brace.
// Consumes the right brace, but nothing after that.
fn parse_block(tokens: &mut TokenIter) -> Result<(Vec<Statement>, Token)> {
    let mut body = Vec::new();
    loop {
        match Statement::parse(tokens, true)? {
            (Some(s), maybe_right_brace) => {
                body.push(s);
                if let Some(brace) = maybe_right_brace {
                    return Ok((body, brace));
                }
            }
            (None, Some(brace)) => {
                return Ok((body, brace));
            }
            (None, None) => {
                return Err(tokens.error("expected statement but got EOF"));
            }
        }
    }
}

// Parse some arguments.
// The first element is an optional template. For example, the <T> in:
// <T>(a: T, f: T -> T)
// The second element is an optional parenthesized list of arguments.
// Finally we have the terminator token.
// Returns the type params, the arguments, and the terminator token.
fn parse_args(
    tokens: &mut TokenIter,
    terminator: TokenType,
) -> Result<(Vec<Token>, Vec<Expression>, Token)> {
    let mut token = Token::expect_token(tokens)?;
    if token.token_type == terminator {
        return Ok((vec![], vec![], token));
    }
    let mut type_params = vec![];
    if token.token_type == TokenType::LessThan {
        loop {
            let token = Token::expect_type(tokens, TokenType::Identifier)?;
            type_params.push(token);
            let token = Token::expect_token(tokens)?;
            match token.token_type {
                TokenType::GreaterThan => {
                    break;
                }
                TokenType::Comma => {
                    continue;
                }
                _ => {
                    return Err(Error::new(
                        &token,
                        "expected '>' or ',' in template type list",
                    ));
                }
            }
        }
        token = Token::expect_token(tokens)?;
    }
    if token.token_type != TokenType::LeftParen {
        return Err(Error::new(&token, "expected an argument list"));
    }
    // Parse the arguments list
    let mut args = Vec::new();
    loop {
        let (exp, t) = Expression::parse(tokens, false, |t| {
            t == TokenType::Comma || t == TokenType::RightParen
        })?;
        args.push(exp);
        if t.token_type == TokenType::RightParen {
            let terminator = Token::expect_type(tokens, terminator)?;
            return Ok((type_params, args, terminator));
        }
    }
}

// Parses a theorem where the keyword identifier (axiom or theorem) has already been found.
// "axiomatic" is whether this is an axiom.
fn parse_theorem_statement(
    keyword: Token,
    tokens: &mut TokenIter,
    axiomatic: bool,
) -> Result<Statement> {
    let token = Token::expect_type(tokens, TokenType::Identifier)?;
    let name = token.text().to_string();
    if !Token::is_valid_variable_name(&name) {
        return Err(Error::new(&token, "invalid theorem name"));
    }
    let (type_params, args, _) = parse_args(tokens, TokenType::Colon)?;
    if type_params.len() > 1 {
        return Err(Error::new(
            &type_params[1],
            "only one type parameter is supported",
        ));
    }
    Token::skip_newlines(tokens);
    let (claim, mut terminator) = Expression::parse(tokens, true, |t| {
        t == TokenType::NewLine || t == TokenType::By
    })?;
    // Let the "by" be after one newline
    match tokens.peek() {
        Some(token) => {
            if token.token_type == TokenType::By {
                terminator = tokens.next().unwrap();
            }
        }
        None => {}
    };
    let (body, last_token) = if terminator.token_type == TokenType::By {
        let left_brace = Token::expect_type(tokens, TokenType::LeftBrace)?;
        let (statements, right_brace) = parse_block(tokens)?;
        let body = Body {
            left_brace,
            statements,
            right_brace: right_brace.clone(),
        };
        (Some(body), right_brace)
    } else {
        (None, terminator)
    };
    let ts = TheoremStatement {
        axiomatic,
        name,
        type_params,
        args,
        claim,
        body,
    };
    let statement = Statement {
        first_token: keyword,
        last_token: last_token,
        statement: StatementInfo::Theorem(ts),
    };
    Ok(statement)
}

// Parses a let statement where the "let" keyword has already been found.
fn parse_let_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let name_token = match tokens.next() {
        Some(t) => t,
        None => return Err(tokens.error("unexpected end of file")),
    };
    match name_token.token_type {
        TokenType::Identifier | TokenType::Number => {}
        _ => {
            return Err(Error::new(&name_token, "expected identifier or number"));
        }
    };
    let name = name_token.text().to_string();
    if !Token::is_valid_variable_name(&name) {
        return Err(Error::new(&keyword, "invalid variable name"));
    }
    Token::expect_type(tokens, TokenType::Colon)?;
    let (type_expr, _) = Expression::parse(tokens, false, |t| t == TokenType::Equals)?;
    let (value, last_token) = Expression::parse(tokens, true, |t| t == TokenType::NewLine)?;
    let ls = LetStatement {
        name,
        name_token,
        type_expr,
        value,
    };
    Ok(Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Let(ls),
    })
}

// Parses a define statement where the "define" keyword has already been found.
fn parse_define_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let name_token = Token::expect_type(tokens, TokenType::Identifier)?;
    let name = name_token.text().to_string();
    if !Token::is_valid_variable_name(&name) {
        return Err(Error::new(&keyword, "invalid variable name"));
    }
    let (type_params, args, _) = parse_args(tokens, TokenType::RightArrow)?;
    if type_params.len() > 1 {
        return Err(Error::new(
            &type_params[1],
            "only one type parameter is supported",
        ));
    }
    let (return_type, _) = Expression::parse(tokens, false, |t| t == TokenType::Colon)?;
    let (return_value, last_token) = Expression::parse(tokens, true, |t| t == TokenType::NewLine)?;
    let ds = DefineStatement {
        name,
        name_token,
        type_params,
        args,
        return_type,
        return_value,
    };
    let statement = Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Define(ds),
    };
    Ok(statement)
}

// Parses a type statement where the "type" keyword has already been found.
fn parse_type_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let name_token = Token::expect_type(tokens, TokenType::Identifier)?;
    let name = name_token.text().to_string();
    if !Token::is_valid_type_name(&name) {
        return Err(Error::new(&name_token, "invalid type name"));
    }
    Token::expect_type(tokens, TokenType::Colon)?;
    Token::skip_newlines(tokens);
    let (type_expr, _) = Expression::parse(tokens, false, |t| t == TokenType::NewLine)?;
    let last_token = type_expr.last_token().clone();
    let ts = TypeStatement { name, type_expr };
    let statement = Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Type(ts),
    };
    Ok(statement)
}

// Parses a forall statement where the "forall" keyword has already been found.
fn parse_forall_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let (_, quantifiers, left_brace) = parse_args(tokens, TokenType::LeftBrace)?;
    let (statements, right_brace) = parse_block(tokens)?;
    let body = Body {
        left_brace,
        statements,
        right_brace: right_brace.clone(),
    };
    let fas = ForAllStatement { quantifiers, body };
    let statement = Statement {
        first_token: keyword,
        last_token: right_brace,
        statement: StatementInfo::ForAll(fas),
    };
    Ok(statement)
}

// If there is an "else { ...statements }" body, parse and consume it.
// Returns None and consumes nothing if there is not an "else" body here.
fn parse_else_body(tokens: &mut TokenIter) -> Result<Option<Body>> {
    loop {
        match tokens.peek() {
            Some(token) => match token.token_type {
                TokenType::NewLine => {
                    tokens.next();
                }
                TokenType::Else => {
                    tokens.next();
                    break;
                }
                _ => return Ok(None),
            },
            None => return Ok(None),
        }
    }
    let left_brace = Token::expect_type(tokens, TokenType::LeftBrace)?;
    let (statements, right_brace) = parse_block(tokens)?;
    let body = Body {
        left_brace,
        statements,
        right_brace,
    };
    Ok(Some(body))
}

// Parses an if statement where the "if" keyword has already been found.
fn parse_if_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let token = tokens.peek().unwrap().clone();
    let (condition, left_brace) = Expression::parse(tokens, true, |t| t == TokenType::LeftBrace)?;
    let (statements, right_brace) = parse_block(tokens)?;
    let body = Body {
        left_brace,
        statements,
        right_brace: right_brace.clone(),
    };
    let else_body = parse_else_body(tokens)?;
    let is = IfStatement {
        condition,
        body,
        else_body,
        token,
    };
    let statement = Statement {
        first_token: keyword,
        last_token: right_brace,
        statement: StatementInfo::If(is),
    };
    Ok(statement)
}

// Parses an exists statement where the "exists" keyword has already been found.
fn parse_exists_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let (_, quantifiers, _) = parse_args(tokens, TokenType::LeftBrace)?;
    let (condition, last_token) = Expression::parse(tokens, true, |t| t == TokenType::RightBrace)?;
    let es = ExistsStatement {
        quantifiers,
        claim: condition,
    };
    let statement = Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Exists(es),
    };
    Ok(statement)
}

// Parses a struct statement where the "struct" keyword has already been found.
fn parse_struct_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let name_token = Token::expect_type(tokens, TokenType::Identifier)?;
    let name = name_token.text().to_string();
    if !Token::is_valid_type_name(&name) {
        return Err(Error::new(&name_token, "invalid struct name"));
    }
    Token::expect_type(tokens, TokenType::LeftBrace)?;
    let mut fields = Vec::new();
    loop {
        let token = Token::expect_token(tokens)?;
        match token.token_type {
            TokenType::NewLine => {
                continue;
            }
            TokenType::RightBrace => {
                if fields.len() == 0 {
                    return Err(Error::new(&token, "structs must have at least one field"));
                }
                return Ok(Statement {
                    first_token: keyword,
                    last_token: token,
                    statement: StatementInfo::Struct(StructStatement {
                        name,
                        name_token,
                        fields,
                    }),
                });
            }
            TokenType::Identifier => {
                Token::expect_type(tokens, TokenType::Colon)?;
                let (type_expr, t) = Expression::parse(tokens, false, |t| {
                    t == TokenType::NewLine || t == TokenType::RightBrace
                })?;
                if t.token_type == TokenType::RightBrace {
                    return Err(Error::new(&t, "field declarations must end with a newline"));
                }
                fields.push((token, type_expr));
            }
            _ => {
                return Err(Error::new(&token, "expected field name"));
            }
        }
    }
}

// Parses a module component list, like "foo.bar.baz".
// Expects to consume a terminator token at the end.
// Returns the strings, along with the token right before the terminator.
fn parse_module_components(
    tokens: &mut TokenIter,
    terminator: TokenType,
) -> Result<(Vec<String>, Token)> {
    let mut components = Vec::new();
    let last_token = loop {
        let token = Token::expect_type(tokens, TokenType::Identifier)?;
        components.push(token.text().to_string());
        let token = Token::expect_token(tokens)?;
        if token.token_type == terminator {
            break token;
        }
        match token.token_type {
            TokenType::Dot => continue,
            _ => return Err(Error::new(&token, "unexpected token in module path")),
        }
    };
    Ok((components, last_token))
}

// Parses an import statement where the "import" keyword has already been found.
fn parse_import_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let (components, last_token) = parse_module_components(tokens, TokenType::NewLine)?;
    let is = ImportStatement {
        components,
        names: vec![],
    };
    let statement = Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Import(is),
    };
    Ok(statement)
}

// Parses a "from" statement where the "from" keyword has already been found.
fn parse_from_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let (components, _) = parse_module_components(tokens, TokenType::Import)?;
    let mut names = vec![];
    let last_token = loop {
        let token = Token::expect_type(tokens, TokenType::Identifier)?;
        let separator = Token::expect_token(tokens)?;
        match separator.token_type {
            TokenType::NewLine => {
                names.push(token.clone());
                break token;
            }
            TokenType::Comma => {
                names.push(token);
                continue;
            }
            _ => {
                return Err(Error::new(&token, "expected comma or newline"));
            }
        }
    };
    let is = ImportStatement { components, names };
    let statement = Statement {
        first_token: keyword,
        last_token,
        statement: StatementInfo::Import(is),
    };
    Ok(statement)
}

// Parses a class statement where the "class" keyword has already been found.
fn parse_class_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let name_token = Token::expect_type(tokens, TokenType::Identifier)?;
    let name = name_token.text().to_string();
    if !Token::is_valid_type_name(&name) {
        return Err(Error::new(&name_token, "invalid class name"));
    }
    let left_brace = Token::expect_type(tokens, TokenType::LeftBrace)?;
    let (statements, right_brace) = parse_block(tokens)?;
    let body = Body {
        left_brace,
        statements,
        right_brace: right_brace.clone(),
    };
    let cs = ClassStatement {
        name,
        name_token,
        body,
    };
    let statement = Statement {
        first_token: keyword,
        last_token: right_brace,
        statement: StatementInfo::Class(cs),
    };
    Ok(statement)
}

// Parses a solve statement where the "solve" keyword has already been found.
fn parse_solve_statement(keyword: Token, tokens: &mut TokenIter) -> Result<Statement> {
    let (target, _) = Expression::parse(tokens, true, |t| t == TokenType::By)?;
    let left_brace = Token::expect_type(tokens, TokenType::LeftBrace)?;
    let (statements, right_brace) = parse_block(tokens)?;
    let body = Body {
        left_brace,
        statements,
        right_brace: right_brace.clone(),
    };
    let ss = SolveStatement { target, body };
    let s = Statement {
        first_token: keyword,
        last_token: right_brace,
        statement: StatementInfo::Solve(ss),
    };
    Ok(s)
}

fn write_type_params(f: &mut fmt::Formatter, type_params: &[Token]) -> fmt::Result {
    if type_params.len() == 0 {
        return Ok(());
    }
    write!(f, "<")?;
    for (i, param) in type_params.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{}", param)?;
    }
    write!(f, ">")?;
    Ok(())
}

fn write_args(f: &mut fmt::Formatter, args: &[Expression]) -> fmt::Result {
    if args.len() == 0 {
        return Ok(());
    }
    write!(f, "(")?;
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{}", arg)?;
    }
    write!(f, ")")?;
    Ok(())
}

impl Statement {
    fn fmt_helper(&self, f: &mut fmt::Formatter, indentation: &str) -> fmt::Result {
        write!(f, "{}", indentation)?;
        match &self.statement {
            StatementInfo::Let(ls) => {
                write!(f, "let {}: {} = {}", ls.name, ls.type_expr, ls.value)
            }

            StatementInfo::Define(ds) => {
                write!(f, "define {}", ds.name)?;
                write_type_params(f, &ds.type_params)?;
                write_args(f, &ds.args)?;
                write!(f, " -> {}: {}", ds.return_type, ds.return_value)
            }

            StatementInfo::Theorem(ts) => {
                if ts.axiomatic {
                    write!(f, "axiom")?;
                } else {
                    write!(f, "theorem")?;
                }
                write!(f, " {}", ts.name)?;
                write_type_params(f, &ts.type_params)?;
                write_args(f, &ts.args)?;
                write!(f, ": {}", ts.claim)?;
                if let Some(body) = &ts.body {
                    write!(f, " by")?;
                    write_block(f, &body.statements, indentation)?;
                }
                Ok(())
            }

            StatementInfo::Prop(ps) => {
                write!(f, "{}", ps.claim)?;
                Ok(())
            }

            StatementInfo::Type(ts) => {
                write!(f, "type {}: {}", ts.name, ts.type_expr)
            }

            StatementInfo::ForAll(fas) => {
                write!(f, "forall")?;
                write_args(f, &fas.quantifiers)?;
                write_block(f, &fas.body.statements, indentation)
            }

            StatementInfo::If(is) => {
                write!(f, "if {}", is.condition)?;
                write_block(f, &is.body.statements, indentation)?;
                if let Some(else_body) = &is.else_body {
                    write!(f, " else")?;
                    write_block(f, &else_body.statements, indentation)?;
                }
                Ok(())
            }

            StatementInfo::Exists(es) => {
                let new_indentation = add_indent(indentation);
                write!(f, "exists")?;
                write_args(f, &es.quantifiers)?;
                write!(
                    f,
                    " {{\n{}{}\n{}}}",
                    &new_indentation, es.claim, indentation
                )
            }

            StatementInfo::Struct(ss) => {
                let new_indentation = add_indent(indentation);
                write!(f, "struct {} {{\n", ss.name)?;
                for (name, type_expr) in &ss.fields {
                    write!(f, "{}{}: {}\n", new_indentation, name, type_expr)?;
                }
                write!(f, "{}}}", indentation)
            }

            StatementInfo::Import(is) => {
                if is.names.is_empty() {
                    write!(f, "import {}", is.components.join("."))
                } else {
                    let names = is
                        .names
                        .iter()
                        .map(|t| t.text())
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, "from {} import {}", is.components.join("."), names)
                }
            }

            StatementInfo::Class(cs) => {
                write!(f, "class {}", cs.name)?;
                write_block(f, &cs.body.statements, indentation)
            }

            StatementInfo::Default(ds) => {
                write!(f, "default {}", ds.type_expr)
            }

            StatementInfo::Solve(ss) => {
                write!(f, "solve {} by", ss.target)?;
                write_block(f, &ss.body.statements, indentation)
            }
        }
    }

    // Tries to parse a single statement from the provided tokens.
    // If in_block is true, we might get a right brace instead of a statement.
    // Returns statement, as well as the right brace token, if the current block ended.
    //
    // Normally, this function consumes the final newline.
    // When it's a right brace that ends a block, though, the last token consumed is the right brace.
    pub fn parse(
        tokens: &mut TokenIter,
        in_block: bool,
    ) -> Result<(Option<Statement>, Option<Token>)> {
        loop {
            if let Some(token) = tokens.peek() {
                match token.token_type {
                    TokenType::NewLine => {
                        tokens.next();
                        continue;
                    }
                    TokenType::Let => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_let_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Axiom => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_theorem_statement(keyword, tokens, true)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Theorem => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_theorem_statement(keyword, tokens, false)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Define => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_define_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Type => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_type_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::RightBrace => {
                        if !in_block {
                            return Err(Error::new(token, "unmatched right brace at top level"));
                        }
                        let brace = tokens.next().unwrap();

                        return Ok((None, Some(brace)));
                    }
                    TokenType::ForAll => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_forall_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::If => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_if_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Exists => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_exists_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Struct => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_struct_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Import => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_import_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Class => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_class_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Default => {
                        let keyword = tokens.next().unwrap();
                        let (type_expr, last_token) =
                            Expression::parse(tokens, true, |t| t == TokenType::NewLine)?;
                        let ds = DefaultStatement { type_expr };
                        let s = Statement {
                            first_token: keyword,
                            last_token,
                            statement: StatementInfo::Default(ds),
                        };
                        return Ok((Some(s), None));
                    }
                    TokenType::From => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_from_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    TokenType::Solve => {
                        let keyword = tokens.next().unwrap();
                        let s = parse_solve_statement(keyword, tokens)?;
                        return Ok((Some(s), None));
                    }
                    _ => {
                        if !in_block {
                            return Err(Error::new(token, "unexpected token at the top level"));
                        }
                        let first_token = tokens.peek().unwrap().clone();
                        let (claim, token) = Expression::parse(tokens, true, |t| {
                            t == TokenType::NewLine || t == TokenType::RightBrace
                        })?;
                        let block_ended = token.token_type == TokenType::RightBrace;
                        let brace = if block_ended { Some(token) } else { None };
                        let last_token = claim.last_token().clone();
                        let se = StatementInfo::Prop(PropStatement { claim });
                        let s = Statement {
                            first_token,
                            last_token,
                            statement: se,
                        };
                        return Ok((Some(s), brace));
                    }
                }
            } else {
                return Ok((None, None));
            }
        }
    }

    #[cfg(test)]
    pub fn parse_str(input: &str) -> Result<Statement> {
        let tokens = Token::scan(input);
        let mut tokens = TokenIter::new(tokens);
        match Statement::parse(&mut tokens, false)? {
            (Some(statement), _) => Ok(statement),
            _ => panic!("expected statement, got EOF"),
        }
    }

    pub fn range(&self) -> Range {
        Range {
            start: self.first_token.start_pos(),
            end: self.last_token.end_pos(),
        }
    }

    pub fn first_line(&self) -> u32 {
        self.first_token.start_pos().line
    }

    pub fn last_line(&self) -> u32 {
        self.last_token.end_pos().line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    fn should_parse(input: &str) -> Statement {
        match Statement::parse_str(input) {
            Ok(statement) => statement,
            Err(e) => panic!("failed to parse {}: {}", input, e),
        }
    }

    fn ok(input: &str) {
        let statement = should_parse(input);
        assert_eq!(input, statement.to_string());
    }

    // Expects an error parsing the input into a statement, but not a lex error
    fn fail(input: &str) {
        if Statement::parse_str(input).is_ok() {
            panic!("statement parsed okay but we expected error:\n{}\n", input);
        }
    }

    #[test]
    fn test_definition_statements() {
        ok("let a: int = x + 2");
        ok("let f: int -> bool = compose(g, h)");
        ok("let f: int -> int = x -> x + 1");
        ok("let g: (int, int, int) -> bool = swap(h)");
        ok("define or(p: bool, q: bool) -> bool: (!p -> q)");
        ok("define and(p: bool, q: bool) -> bool: !(p -> !q)");
        ok("define iff(p: bool, q: bool) -> bool: (p -> q) & (q -> p)");
    }

    #[test]
    fn test_theorem_statements() {
        ok("axiom simplification: p -> (q -> p)");
        ok("axiom distribution: (p -> (q -> r)) -> ((p -> q) -> (p -> r))");
        ok("axiom contraposition: (!p -> !q) -> (q -> p)");
        ok("axiom modus_ponens(p, p -> q): q");
        ok("theorem and_comm: p & q <-> q & p");
        ok("theorem and_assoc: (p & q) & r <-> p & (q & r)");
        ok("theorem or_comm: p | q <-> q | p");
        ok("theorem or_assoc: (p | q) | r <-> p | (q | r)");
        ok("theorem suc_gt_zero(x: nat): suc(x) > 0");
    }

    #[test]
    fn test_prop_statements() {
        ok(indoc! {"
        theorem goal: true by {
            p -> p
        }"});
    }

    #[test]
    fn test_forall_statements() {
        ok(indoc! {"
            forall(x: Nat) {
                f(x) -> f(suc(x))
            }"});
    }

    #[test]
    fn test_forall_value_in_statement() {
        ok("let p: bool = forall(b: bool) { b | !b }");
    }

    #[test]
    fn test_nat_ac_statements() {
        ok("type Nat: axiom");
        ok("let suc: Nat -> Nat = axiom");
        ok("axiom suc_injective(x: Nat, y: Nat): suc(x) = suc(y) -> x = y");
        ok("axiom suc_neq_zero(x: Nat): suc(x) != 0");
        ok("axiom induction(f: Nat -> bool, n: Nat): f(0) & forall(k: Nat) { f(k) -> f(suc(k)) } -> f(n)");
        ok("define recursion(f: Nat -> Nat, a: Nat, n: Nat) -> Nat: axiom");
        ok("axiom recursion_base(f: Nat -> Nat, a: Nat): recursion(f, a, 0) = a");
        ok("axiom recursion_step(f: Nat -> Nat, a: Nat, n: Nat): recursion(f, a, suc(n)) = f(recursion(f, a, n))");
        ok("define add(x: Nat, y: Nat) -> Nat: recursion(suc, x, y)");
        ok("theorem add_zero_right(a: Nat): add(a, 0) = a");
        ok("theorem add_zero_left(a: Nat): add(0, a) = a");
        ok("theorem add_suc_right(a: Nat, b: Nat): add(a, suc(b)) = suc(add(a, b))");
        ok("theorem add_suc_left(a: Nat, b: Nat): add(suc(a), b) = suc(add(a, b))");
        ok("theorem add_comm(a: Nat, b: Nat): add(a, b) = add(b, a)");
        ok("theorem add_assoc(a: Nat, b: Nat, c: Nat): add(a, add(b, c)) = add(add(a, b), c)");
    }

    #[test]
    fn test_multiline_at_colon() {
        Statement::parse_str("type Nat:\n  axiom").unwrap();
        Statement::parse_str("theorem foo(b: Bool):\nb | !b").unwrap();
    }

    #[test]
    fn test_block_parsing() {
        ok(indoc! {"
            theorem foo: bar by {
                baz
            }"});
        fail(indoc! {"
            foo {
                bar
            }"});
    }

    #[test]
    fn test_by_on_next_line() {
        let statement = should_parse(indoc! {"
            theorem foo: bar
            by {
                baz
            }"});
        let expected = indoc! {"
        theorem foo: bar by {
            baz
        }"};
        assert_eq!(expected, statement.to_string());
    }

    #[test]
    fn test_statement_errors() {
        fail("+ + +");
        fail("let p: Bool =");
        fail("let p: Bool = (");
        fail("let p: Bool = (x + 2");
        fail("let p: Bool = x + 2)");
    }

    #[test]
    fn test_declared_variable_names_lowercased() {
        ok("let p: Bool = true");
        fail("let P: Bool = true");
    }

    #[test]
    fn test_defined_variable_names_lowercased() {
        ok("define foo(x: Bool) -> Bool: true");
        fail("define Foo(x: Bool) -> Bool: true");
    }

    #[test]
    fn test_theorem_names_lowercased() {
        ok("theorem foo: true");
        fail("theorem Foo: true");
    }

    #[test]
    fn test_struct_names_titlecased() {
        ok(indoc! {"
        struct Foo {
            bar: Nat
        }"});
        fail(indoc! {"
        struct foo {
            bar: Nat
        }"});
    }

    #[test]
    fn test_type_names_titlecased() {
        ok("type Foo: axiom");
        fail("type foo: axiom");
    }

    #[test]
    fn test_only_declarations_in_signatures() {
        fail("theorem foo(x: int, x > 0): x + 1 > 0");
    }

    #[test]
    fn test_single_line_forall() {
        should_parse("forall(x: Nat) { f(x) -> f(suc(x)) }");
    }

    #[test]
    fn test_exists_statement() {
        ok(indoc! {"
        exists(x: Nat) {
            x > 0
        }"});
    }

    #[test]
    fn test_single_line_exists_statement() {
        should_parse("exists(x: Nat) { x > 0 }");
    }

    #[test]
    fn test_if_statement() {
        ok(indoc! {"
        if x > 1 {
            x > 0
        }"});
    }

    #[test]
    fn test_struct_statement() {
        ok(indoc! {"
        struct NatPair {
            first: Nat
            second: Nat
        }"});
    }

    #[test]
    fn test_no_empty_structs() {
        fail("struct Foo {}");
    }

    #[test]
    fn test_struct_fields_need_newlines() {
        fail("struct Foo { bar: Nat }");
    }

    #[test]
    fn test_parametric_theorem() {
        ok("axiom recursion_base<T>(f: T -> T, a: T): recursion(f, a, 0) = a");
    }

    #[test]
    fn test_parametric_definition() {
        ok("define recursion<T>(f: T -> T, a: T, n: Nat) -> Nat: axiom");
    }

    #[test]
    fn test_import_statement() {
        ok("import foo.bar.baz");
    }

    #[test]
    fn test_if_else_statement() {
        ok(indoc! {"
        if foo(x) {
            bar(x)
        } else {
            qux(x)
        }"});
    }

    #[test]
    fn test_no_lone_else_statement() {
        fail("else { qux(x) }");
    }

    #[test]
    fn test_class_statement() {
        ok(indoc! {"
        class Foo {
            let blorp: Foo = axiom
            let 0: Foo = axiom
        }"});
    }

    #[test]
    fn test_from_statement() {
        ok("from foo import bar");
        ok("from foo.bar import baz");
        ok("from foo.bar.qux import baz, zip");
        fail("from foo");
    }

    #[test]
    fn test_solve_statement() {
        ok(indoc! {"
        solve x by {
            x = 2
        }"});
    }
}

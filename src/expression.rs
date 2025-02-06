use std::{collections::VecDeque, fmt};

use tower_lsp::lsp_types::Range;

use crate::compilation::{Error, ErrorSource, Result};
use crate::token::{Token, TokenIter, TokenType};

// There are two sorts of expressions.
// Value expressions, like:
//    1 + 2
// Type expressions, like:
//    (int, bool) -> bool
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ExpressionType {
    Value,
    Type,
}

// An Expression represents the basic structuring of tokens into a syntax tree.
// The expression does not typecheck and enforce semantics; it's just parsing into a tree.
#[derive(Debug)]
pub enum Expression {
    // A singleton expression is one that consists of just a single token.
    // This includes identifiers, true, false, numeric literals, and "axiom".
    Singleton(Token),

    // A unary operator applied to another expression.
    Unary(Token, Box<Expression>),

    // An infix binary operator, with the left and right expressions.
    Binary(Box<Expression>, Token, Box<Expression>),

    // Expressions you get by placing two expressions next to each other.
    // Typically the application of a function.
    // The second parameter can either be an argument list or a type parameter list.
    Apply(Box<Expression>, Box<Expression>),

    // A grouping like ( <expr> ) or { <expr> } or < <expr> >.
    // The tokens of the bracey things that delimit the grouping are included.
    Grouping(Token, Box<Expression>, Token),

    // A binder is an expression that binds variables, like a forall/exists/function.
    // The first token is the binder keyword, like "forall".
    // The declarations are the argument list, like "(x: Nat, y: Nat)".
    // The expression is the body block.
    // The last token is the closing brace.
    Binder(Token, Vec<Declaration>, Box<Expression>, Token),

    // If-then-else expressions have to have the else block.
    // The first token is the "if" keyword.
    // The first expression is the condition.
    // The second expression is the "if" block.
    // The third expression is the "else" block.
    // The last token is the closing brace.
    IfThenElse(
        Token,
        Box<Expression>,
        Box<Expression>,
        Box<Expression>,
        Token,
    ),

    // The first token is the "match" keyword.
    // The first expression is the "scrutinee", which we are matching.
    // The pairs indicate exhaustive and mutually exclusive cases for matching the scrutinee, which
    // must have an inductive type.
    // For the pair (exp1, exp2) where exp1 matches the scrutinee, the value of our expression is exp2.
    // The last token is the closing brace.
    Match(Token, Box<Expression>, Vec<(Expression, Expression)>, Token),
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Expression::Singleton(token) => write!(f, "{}", token),
            Expression::Unary(token, subexpression) => {
                if token.token_type == TokenType::Minus {
                    write!(f, "{}{}", token, subexpression)
                } else {
                    write!(f, "{} {}", token, subexpression)
                }
            }
            Expression::Binary(left, token, right) => {
                let left_spacer = if token.token_type.left_space() {
                    " "
                } else {
                    ""
                };
                let right_spacer = if token.token_type.right_space() {
                    " "
                } else {
                    ""
                };
                write!(
                    f,
                    "{}{}{}{}{}",
                    left, left_spacer, token, right_spacer, right
                )
            }
            Expression::Apply(left, right) => {
                write!(f, "{}{}", left, right)
            }
            Expression::Grouping(_, e, _) => {
                write!(f, "({})", e)
            }
            Expression::Binder(token, args, sub, _) => {
                write!(f, "{}", token)?;
                Declaration::write_vec(f, args)?;
                write!(f, " {{ {} }}", sub)
            }
            Expression::IfThenElse(_, cond, if_block, else_block, _) => {
                write!(
                    f,
                    "if {} {{ {} }} else {{ {} }}",
                    cond, if_block, else_block
                )
            }
            Expression::Match(_, scrutinee, cases, _) => {
                write!(f, "match {} {{", scrutinee)?;
                for (pat, exp) in cases {
                    write!(f, " {} {{ {} }}", pat, exp)?;
                }
                write!(f, " }}")
            }
        }
    }
}

impl ErrorSource for Expression {
    fn error(&self, message: &str) -> Error {
        Error::new(self.first_token(), self.last_token(), message)
    }
}

// A single variable declaration, like "p: bool".
#[derive(Debug)]
pub enum Declaration {
    // (name token, type expression)
    Typed(Token, Expression),

    // Just the token 'self'.
    SelfToken(Token),
}

impl fmt::Display for Declaration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Declaration::Typed(name_token, type_expr) => {
                write!(f, "{}: {}", name_token, type_expr)
            }
            Declaration::SelfToken(token) => write!(f, "{}", token),
        }
    }
}

impl Declaration {
    pub fn token(&self) -> &Token {
        match self {
            Declaration::Typed(token, _) => token,
            Declaration::SelfToken(token) => token,
        }
    }

    // Parses an expression that should contain a single declaration.
    // This rejects numerals.
    pub fn parse(tokens: &mut TokenIter, terminator: Terminator) -> Result<(Declaration, Token)> {
        let name_token = tokens.expect_variable_name(false)?;
        if name_token.text() == "self" {
            let token = tokens.expect_token()?;
            if token.token_type == TokenType::Colon {
                return Err(token.error("no type is needed after 'self'"));
            }
            if !terminator.matches(&token.token_type) {
                return Err(
                    token.error(&format!("expected {} but found \"{}\"", terminator, token))
                );
            }
            return Ok((Declaration::SelfToken(name_token.clone()), token));
        }
        tokens.expect_type(TokenType::Colon)?;
        let (type_expr, token) = Expression::parse_type(tokens, terminator)?;

        Ok((Declaration::Typed(name_token, type_expr), token))
    }

    // Parses a declaration list, after the opening left parenthesis has already been consumed.
    // Consumes a closing right paren.
    // Returns the declarations.
    pub fn parse_list(tokens: &mut TokenIter) -> Result<Vec<Declaration>> {
        let mut declarations = Vec::new();
        loop {
            let (declaration, last_token) = Declaration::parse(
                tokens,
                Terminator::Or(TokenType::Comma, TokenType::RightParen),
            )?;
            declarations.push(declaration);
            if last_token.token_type == TokenType::RightParen {
                return Ok(declarations);
            }
        }
    }

    fn write_vec(f: &mut fmt::Formatter, decls: &Vec<Declaration>) -> fmt::Result {
        let mut first = true;
        for decl in decls {
            if first {
                write!(f, "(")?;
                first = false;
            } else {
                write!(f, ", ")?;
            }
            write!(f, "{}", decl)?;
        }
        write!(f, ")")
    }
}

// We use terminators to tell the expression parser when it is allowed to stop.
// This exists to make error messages more readable.
pub enum Terminator {
    Is(TokenType),
    Or(TokenType, TokenType),
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Terminator::Is(t) => write!(f, "{}", t.describe()),
            Terminator::Or(t1, t2) => write!(f, "{} or {}", t1.describe(), t2.describe()),
        }
    }
}

impl Terminator {
    fn matches(&self, t: &TokenType) -> bool {
        match self {
            Terminator::Is(t1) => t == t1,
            Terminator::Or(t1, t2) => t == t1 || t == t2,
        }
    }
}

impl Expression {
    pub fn first_token(&self) -> &Token {
        match self {
            Expression::Singleton(token) => token,
            Expression::Unary(token, _) => token,
            Expression::Binary(left, _, _) => left.first_token(),
            Expression::Apply(left, _) => left.first_token(),
            Expression::Grouping(left_paren, _, _) => left_paren,
            Expression::Binder(token, _, _, _) => token,
            Expression::IfThenElse(token, _, _, _, _) => token,
            Expression::Match(token, _, _, _) => token,
        }
    }

    pub fn last_token(&self) -> &Token {
        match self {
            Expression::Singleton(token) => token,
            Expression::Unary(_, subexpression) => subexpression.last_token(),
            Expression::Binary(_, _, right) => right.last_token(),
            Expression::Apply(_, right) => right.last_token(),
            Expression::Grouping(_, _, right_paren) => right_paren,
            Expression::Binder(_, _, _, right_brace) => right_brace,
            Expression::IfThenElse(_, _, _, _, right_brace) => right_brace,
            Expression::Match(_, _, _, right_brace) => right_brace,
        }
    }

    pub fn is_axiom(&self) -> bool {
        match self {
            Expression::Singleton(token) => token.token_type == TokenType::Axiom,
            _ => false,
        }
    }

    pub fn print_one_level(&self) {
        match self {
            Expression::Singleton(token) => {
                println!("Singleton:");
                println!("  token: {}", token);
            }
            Expression::Unary(token, subexpression) => {
                println!("Unary:");
                println!("  token: {}", token);
                println!("  subexpression: {}", subexpression);
            }
            Expression::Binary(left, token, right) => {
                println!("Binary:");
                println!("  token: {}", token);
                println!("  left: {}", left);
                println!("  right: {}", right);
            }
            Expression::Apply(left, right) => {
                println!("Apply:");
                println!("  left: {}", left);
                println!("  right: {}", right);
            }
            Expression::Grouping(_, e, _) => {
                println!("Grouping:");
                println!("  subexpression: {}", e);
            }
            Expression::Binder(token, args, sub, _) => {
                println!("Binder:");
                println!("  token: {}", token);
                println!("  args: {:?}", args);
                println!("  subexpression: {}", sub);
            }
            Expression::IfThenElse(token, cond, if_block, else_block, _) => {
                println!("IfThenElse:");
                println!("  token: {}", token);
                println!("  cond: {}", cond);
                println!("  if: {}", if_block);
                println!("  else: {}", else_block);
            }
            Expression::Match(token, scrutinee, cases, _) => {
                println!("Match:");
                println!("  token: {}", token);
                println!("  scrutinee: {}", scrutinee);
                for (pat, exp) in cases {
                    println!("  case: {} => {}", pat, exp);
                }
            }
        }
    }

    // For code generation. Will not point to a token in any larger document
    pub fn generate_identifier(s: &str) -> Expression {
        Expression::Singleton(TokenType::Identifier.new_token(s))
    }

    pub fn generate_identifier_chain(parts: &[&str]) -> Expression {
        let mut answer = Expression::generate_identifier(parts[0]);
        for part in &parts[1..] {
            answer = Expression::Binary(
                Box::new(answer),
                TokenType::Dot.generate(),
                Box::new(Expression::generate_identifier(part)),
            );
        }
        answer
    }

    // Generates a comma-separated grouping
    pub fn generate_grouping(mut exprs: Vec<Expression>) -> Expression {
        assert_ne!(exprs.len(), 0);
        let mut answer = exprs.remove(0);
        for e in exprs {
            answer = Expression::Binary(Box::new(answer), TokenType::Comma.generate(), Box::new(e));
        }
        Expression::Grouping(
            TokenType::LeftParen.generate(),
            Box::new(answer),
            TokenType::RightParen.generate(),
        )
    }

    // Generates a unary expression, parenthesizing if necessary according to precedence.
    pub fn generate_unary(op: TokenType, mut expr: Expression) -> Expression {
        if expr.top_level_precedence(true) < op.unary_precedence() {
            expr = Expression::Grouping(
                TokenType::LeftParen.generate(),
                Box::new(expr),
                TokenType::RightParen.generate(),
            );
        }
        Expression::Unary(op.generate(), Box::new(expr))
    }

    // Generates a binary expression, parenthesizing if necessary according to precedence.
    pub fn generate_binary(
        mut left: Expression,
        op: TokenType,
        mut right: Expression,
    ) -> Expression {
        if left.top_level_precedence(false) < op.binary_precedence() {
            left = Expression::Grouping(
                TokenType::LeftParen.generate(),
                Box::new(left),
                TokenType::RightParen.generate(),
            );
        }
        if right.top_level_precedence(true) <= op.binary_precedence() {
            right = Expression::Grouping(
                TokenType::LeftParen.generate(),
                Box::new(right),
                TokenType::RightParen.generate(),
            );
        }
        Expression::Binary(Box::new(left), op.generate(), Box::new(right))
    }

    // Converts this expression to a numeric digit, if possible.
    // Ignores the type.
    pub fn to_digit(&self) -> Option<char> {
        match self {
            Expression::Singleton(token) if token.token_type == TokenType::Numeral => {
                let text = token.text();
                if text.len() == 1 {
                    text.chars().next()
                } else {
                    None
                }
            }
            Expression::Binary(_, token, right) if token.token_type == TokenType::Dot => {
                right.to_digit()
            }
            _ => None,
        }
    }

    // Whether this is a number of any type.
    pub fn is_number(&self) -> bool {
        match self {
            Expression::Singleton(token) => token.token_type == TokenType::Numeral,
            Expression::Binary(_, token, right) if token.token_type == TokenType::Dot => {
                right.is_number()
            }
            _ => false,
        }
    }

    // Appends a digit.
    // 'initial' must be a number.
    pub fn generate_number(initial: Expression, digit: char) -> Expression {
        match initial {
            Expression::Singleton(token) => {
                let mut text = token.text().to_string();
                text.push(digit);
                Expression::Singleton(TokenType::Numeral.new_token(&text))
            }
            Expression::Binary(left, token, right) if token.token_type == TokenType::Dot => {
                let new_right = Expression::generate_number(*right, digit);
                Expression::Binary(left, token, Box::new(new_right))
            }
            _ => panic!("expected a number"),
        }
    }

    // The precedence this expression uses at the top level.
    // In some situations, unary operators will be parsed regardless of precedence, so they essentially
    // use maximum precedence. In this situation `unary_counts_as_max` is true.
    pub fn top_level_precedence(&self, unary_counts_as_max: bool) -> i8 {
        match self {
            Expression::Singleton(_)
            | Expression::Grouping(..)
            | Expression::Binder(..)
            | Expression::IfThenElse(..)
            | Expression::Match(..) => {
                // These expressions never need to be parenthesized.
                i8::MAX
            }
            Expression::Unary(token, _) => {
                if unary_counts_as_max {
                    i8::MAX
                } else {
                    token.unary_precedence()
                }
            }
            Expression::Binary(_, token, _) => token.binary_precedence(),
            Expression::Apply(..) => TokenType::Dot.binary_precedence(),
        }
    }

    // If this expression is of the form "premise -> conclusion", return the premise.
    pub fn premise(&self) -> Option<&Expression> {
        match self {
            Expression::Grouping(_, e, _) => e.premise(),
            Expression::Binary(left, token, _) => {
                if token.token_type == TokenType::RightArrow {
                    Some(left)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn range(&self) -> Range {
        Range {
            start: self.first_token().start_pos(),
            end: self.last_token().end_pos(),
        }
    }

    // Flattens an expression like "1, 2, 3"
    pub fn flatten_comma_separated_list(&self) -> Vec<&Expression> {
        match self {
            Expression::Binary(left, token, right) => {
                if token.token_type == TokenType::Comma {
                    let mut args = left.flatten_comma_separated_list();
                    args.append(&mut right.flatten_comma_separated_list());
                    args
                } else {
                    vec![&self]
                }
            }
            _ => vec![&self],
        }
    }

    // Flattens an expression like "(1, 2, 3)"
    // If allow_singleton is true, then something like "1" also counts as a list.
    // If allow_singleton is false, a list of length 1 must be parenthesized like "(1)".
    pub fn flatten_list(&self, allow_singleton: bool) -> Result<Vec<&Expression>> {
        match self {
            Expression::Grouping(_, e, _) => Ok(e.flatten_comma_separated_list()),
            e => {
                if !allow_singleton {
                    Err(self.error(&format!("expected a grouped list but found: {}", self)))
                } else {
                    Ok(vec![e])
                }
            }
        }
    }

    // Parses a single expression from the provided tokens.
    // termination determines what tokens are allowed to be the terminator.
    // Consumes the terminating token and returns it.
    fn parse(
        tokens: &mut TokenIter,
        expected_type: ExpressionType,
        termination: Terminator,
    ) -> Result<(Expression, Token)> {
        let (partials, terminator) = parse_partial_expressions(tokens, expected_type, termination)?;
        check_partial_expressions(&partials)?;
        let expression = combine_partial_expressions(partials, expected_type, tokens)?;
        Ok((expression, terminator))
    }

    // Parse an expression that should represent a value.
    // Consumes the terminating token and returns it.
    pub fn parse_value(
        tokens: &mut TokenIter,
        terminator: Terminator,
    ) -> Result<(Expression, Token)> {
        Expression::parse(tokens, ExpressionType::Value, terminator)
    }

    // Parse an expression that should represent a type, or part of a type.
    // Consumes the terminating token and returns it.
    pub fn parse_type(
        tokens: &mut TokenIter,
        terminator: Terminator,
    ) -> Result<(Expression, Token)> {
        Expression::parse(tokens, ExpressionType::Type, terminator)
    }

    fn expect_parse(input: &str, expected_type: ExpressionType) -> Expression {
        let tokens = Token::scan(input);
        let mut tokens = TokenIter::new(tokens);
        match Expression::parse(
            &mut tokens,
            expected_type,
            Terminator::Is(TokenType::NewLine),
        ) {
            Ok((e, _)) => e,
            Err(e) => panic!("unexpected error parsing: {}", e),
        }
    }

    pub fn expect_value(input: &str) -> Expression {
        Expression::expect_parse(input, ExpressionType::Value)
    }
}

// When we have a sequence of precedence-based operators, we need to gather the whole
// sequence before combining them.
// The PartialExpressions are what we have before doing this combination.
// The precedence-based operators include unary operators, infix operators, and function application.
#[derive(Debug)]
enum PartialExpression {
    // Already a complete expression
    Expression(Expression),

    // Tokens that are only part of an expression
    Unary(Token),
    Binary(Token),

    // An implicit apply, like "f(x)". It's located between the f and the (x).
    ImplicitApply(Token),
}

impl fmt::Display for PartialExpression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PartialExpression::Expression(e) => write!(f, "{}", e),
            PartialExpression::Unary(token) | PartialExpression::Binary(token) => {
                write!(f, "{}", token)
            }
            PartialExpression::ImplicitApply(_) => write!(f, "<apply>"),
        }
    }
}

impl ErrorSource for PartialExpression {
    fn error(&self, message: &str) -> Error {
        match &self {
            PartialExpression::Expression(e) => e.error(message),
            PartialExpression::Unary(t)
            | PartialExpression::Binary(t)
            | PartialExpression::ImplicitApply(t) => t.error(message),
        }
    }
}

// After a '<' has been consumed, check if the following tokens look like a param list.
// I.e., something like "T, U, V>".
// If it doesn't look like parameters, just return an empty vector.
fn parse_params(less_than: &Token, tokens: &mut TokenIter) -> Result<Option<Expression>> {
    match tokens.peek() {
        Some(t) => {
            if !t.is_type_name() {
                return Ok(None);
            }
        }
        None => {
            return Ok(None);
        }
    }

    let mut expr = Expression::Singleton(tokens.next().unwrap());

    loop {
        let token = tokens.expect_token()?;
        match token.token_type {
            TokenType::Comma => {
                let next_type = tokens.expect_type_name()?;
                expr = Expression::Binary(
                    Box::new(expr),
                    token,
                    Box::new(Expression::Singleton(next_type)),
                );
            }
            TokenType::GreaterThan => {
                let grouping = Expression::Grouping(less_than.clone(), Box::new(expr), token);
                return Ok(Some(grouping));
            }
            _ => {
                return Err(token.error("expected ',' or '>'"));
            }
        }
    }
}

// Create partial expressions from tokens.
// termination determines what tokens are allowed to be the terminator.
// Consumes the terminating token from the iterator and returns it.
fn parse_partial_expressions(
    tokens: &mut TokenIter,
    expected_type: ExpressionType,
    termination: Terminator,
) -> Result<(VecDeque<PartialExpression>, Token)> {
    let mut partials = VecDeque::<PartialExpression>::new();
    while let Some(token) = tokens.next() {
        if termination.matches(&token.token_type) {
            return Ok((partials, token));
        }
        if token.token_type.is_binary() {
            match (expected_type, token.token_type) {
                (ExpressionType::Value, TokenType::Colon) => {
                    return Err(token.error("unexpected colon in value"));
                }
                (ExpressionType::Value, _) => {
                    // Anything else can be in a value
                }
                (ExpressionType::Type, TokenType::Comma)
                | (ExpressionType::Type, TokenType::RightArrow)
                | (ExpressionType::Type, TokenType::Dot) => {
                    // These are okay in types
                }
                (ExpressionType::Type, _) => {
                    return Err(token.error("unexpected token in type"));
                }
            }

            if token.token_type.is_unary() {
                // This could either be unary or binary.
                // Look at the previous token to decide.
                if match partials.back() {
                    Some(PartialExpression::Expression(_)) => {
                        // An expression can't be followed by a unary operator.
                        false
                    }
                    Some(PartialExpression::Unary(_))
                    | Some(PartialExpression::ImplicitApply(_))
                    | Some(PartialExpression::Binary(_))
                    | None => {
                        // All of these things can be followed by a unary operator.
                        true
                    }
                } {
                    partials.push_back(PartialExpression::Unary(token));
                    continue;
                }
            }

            partials.push_back(PartialExpression::Binary(token));
            continue;
        }
        if token.token_type.is_unary() {
            partials.push_back(PartialExpression::Unary(token));
            continue;
        }
        match token.token_type {
            TokenType::LeftParen => {
                let (subexpression, last_token) = Expression::parse(
                    tokens,
                    expected_type,
                    Terminator::Is(TokenType::RightParen),
                )?;

                // A group that has no operator before it gets an implicit apply.
                if matches!(partials.back(), Some(PartialExpression::Expression(_))) {
                    partials.push_back(PartialExpression::ImplicitApply(token.clone()));
                }

                let group = Expression::Grouping(token, Box::new(subexpression), last_token);
                partials.push_back(PartialExpression::Expression(group));
            }

            TokenType::Identifier | TokenType::Axiom => {
                partials.push_back(PartialExpression::Expression(Expression::Singleton(token)));
            }
            TokenType::Numeral | TokenType::True | TokenType::False | TokenType::SelfToken => {
                if expected_type == ExpressionType::Type {
                    return Err(token.error("expected a type but found a value"));
                }
                partials.push_back(PartialExpression::Expression(Expression::Singleton(token)));
            }

            TokenType::ForAll | TokenType::Exists | TokenType::Function => {
                if expected_type != ExpressionType::Value {
                    return Err(token.error("quantifiers cannot be used here"));
                }
                tokens.expect_type(TokenType::LeftParen)?;
                let args = Declaration::parse_list(tokens)?;
                tokens.expect_type(TokenType::LeftBrace)?;
                let (subexpression, right_brace) = Expression::parse(
                    tokens,
                    ExpressionType::Value,
                    Terminator::Is(TokenType::RightBrace),
                )?;
                let binder = Expression::Binder(token, args, Box::new(subexpression), right_brace);
                partials.push_back(PartialExpression::Expression(binder));
            }

            TokenType::If => {
                if expected_type != ExpressionType::Value {
                    return Err(token.error("'if' expressions cannot be used here"));
                }
                let (condition, _) =
                    Expression::parse_value(tokens, Terminator::Is(TokenType::LeftBrace))?;
                let (if_block, _) =
                    Expression::parse_value(tokens, Terminator::Is(TokenType::RightBrace))?;
                tokens.expect_type(TokenType::Else)?;
                tokens.expect_type(TokenType::LeftBrace)?;
                let (else_block, last_right_brace) =
                    Expression::parse_value(tokens, Terminator::Is(TokenType::RightBrace))?;
                let exp = Expression::IfThenElse(
                    token,
                    Box::new(condition),
                    Box::new(if_block),
                    Box::new(else_block),
                    last_right_brace,
                );
                partials.push_back(PartialExpression::Expression(exp));
            }

            TokenType::Match => {
                if expected_type != ExpressionType::Value {
                    return Err(token.error("'match' cannot be used here"));
                }
                let (scrutinee, _) =
                    Expression::parse_value(tokens, Terminator::Is(TokenType::LeftBrace))?;

                let mut cases = vec![];

                let right_brace = loop {
                    tokens.skip_newlines();
                    match tokens.peek() {
                        None => {
                            return Err(tokens.error("expected a case but got EOF"));
                        }
                        Some(t) => {
                            if t.token_type == TokenType::RightBrace {
                                break tokens.next().unwrap();
                            }
                        }
                    }

                    let (pattern, _) =
                        Expression::parse_value(tokens, Terminator::Is(TokenType::LeftBrace))?;
                    let (exp, _) =
                        Expression::parse_value(tokens, Terminator::Is(TokenType::RightBrace))?;
                    cases.push((pattern, exp));
                };
                if cases.is_empty() {
                    return Err(tokens.error("expected at least one case"));
                }
                let exp = Expression::Match(token, Box::new(scrutinee), cases, right_brace);
                partials.push_back(PartialExpression::Expression(exp));
            }

            TokenType::NewLine => {
                // Ignore newlines. The case where the newline is a terminator, we already caught.
            }

            _ => {
                return Err(token.error(&format!(
                    "expected an expression terminated by {}",
                    termination
                )));
            }
        }
    }
    Err(tokens.error("expected expression but got EOF"))
}

// Find the index of the operator that should operate last. (Ie, the root of the tree.)
// If there are no operators, return None.
fn find_last_operator(partials: &VecDeque<PartialExpression>) -> Result<Option<usize>> {
    let operators = partials.iter().enumerate().filter_map(|(i, partial)| {
        match partial {
            PartialExpression::Unary(token) => {
                // Only a unary operator at the beginning of the expression can operate last
                if i == 0 {
                    Some((-token.unary_precedence(), i))
                } else {
                    None
                }
            }
            PartialExpression::Binary(token) => Some((-token.binary_precedence(), i)),
            PartialExpression::ImplicitApply(_) => {
                // Application has the same precedence as dot, so it goes left to right.
                // This is intuitive if you look at the cases:
                // foo.bar.baz is parsed as (foo.bar).baz
                // foo.bar(baz) is parsed as (foo.bar)(baz)
                // foo(bar).baz is parsed as (foo(bar)).baz
                // foo(bar)(baz) is parsed as (foo(bar))(baz)
                Some((-TokenType::Dot.binary_precedence(), i))
            }
            _ => None,
        }
    });

    match operators.max() {
        Some((neg_precedence, index)) => {
            if neg_precedence == 0 {
                return Err(partials[index].error(&format!(
                    "the parser did not expect a precedence-zero thing here"
                )));
            }
            Ok(Some(index))
        }
        None => Ok(None),
    }
}

// Checks to see if the partial expressions are valid.
// This is not necessary for correctness. But we can generate a nicer error message here than
// in the depths of a recursion.
fn check_partial_expressions(partials: &VecDeque<PartialExpression>) -> Result<()> {
    if partials.len() > 1 {
        // Iterate over all pairs
        for i in 0..(partials.len() - 1) {
            let left = &partials[i];
            let right = &partials[i + 1];
            match (left, right) {
                (PartialExpression::Binary(a), PartialExpression::Binary(b))
                | (PartialExpression::Unary(a), PartialExpression::Binary(b)) => {
                    return Err(right.error(&format!(
                        "the '{}' operator cannot be followed by the '{}' operator",
                        a, b
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

// Combines partial expressions into a single expression.
// Operators work in precedence order, and left-to-right within a single precedence.
// This algorithm is quadratic, so perhaps we should improve it at some point.
fn combine_partial_expressions(
    mut partials: VecDeque<PartialExpression>,
    expected_type: ExpressionType,
    iter: &mut TokenIter,
) -> Result<Expression> {
    if partials.len() == 0 {
        return Err(iter.error("expected an expression here"));
    }
    if partials.len() == 1 {
        let partial = partials.pop_back().unwrap();
        if let PartialExpression::Expression(e) = partial {
            return Ok(e);
        }
        return Err(partial.error("incomplete expression"));
    }

    // If there are operators, find the operator that should operate last,
    // and recurse on each of the two sides.
    if let Some(index) = find_last_operator(&partials)? {
        if index == 0 {
            let partial = partials.pop_front().unwrap();
            if let PartialExpression::Unary(token) = partial {
                return Ok(Expression::Unary(
                    token,
                    Box::new(combine_partial_expressions(partials, expected_type, iter)?),
                ));
            }
            return Err(partial.error("expected unary operator"));
        }

        let mut right_partials = partials.split_off(index);
        let partial = right_partials.pop_front().unwrap();

        return match partial {
            PartialExpression::Binary(token) => {
                let left = combine_partial_expressions(partials, expected_type, iter)?;
                let right = combine_partial_expressions(right_partials, expected_type, iter)?;
                Ok(Expression::Binary(Box::new(left), token, Box::new(right)))
            }
            PartialExpression::ImplicitApply(_) => {
                let left = combine_partial_expressions(partials, expected_type, iter)?;
                let right = combine_partial_expressions(right_partials, expected_type, iter)?;
                Ok(Expression::Apply(Box::new(left), Box::new(right)))
            }
            _ => Err(partial.error("expected binary operator")),
        };
    }

    // When there are no operators, the nature of the first partial expression should
    // tell us how to handle the rest of them.
    match partials.pop_front().unwrap() {
        // When the first partial is a normal expression, that looks like a function application.
        PartialExpression::Expression(mut answer) => {
            for partial in partials.into_iter() {
                match partial {
                    PartialExpression::Expression(expr) => match expr {
                        Expression::Grouping(_, _, _) => {
                            answer = Expression::Apply(Box::new(answer), Box::new(expr))
                        }
                        _ => return Err(expr.error("expected a grouping")),
                    },
                    _ => return Err(partial.error("unexpected operator")),
                }
            }
            Ok(answer)
        }

        e => Err(e.error("expected an expression")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_optimal(input: &str, expected_type: ExpressionType) {
        let output = Expression::expect_parse(input, expected_type).to_string();
        assert_eq!(input, output);
    }

    fn check_value(input: &str) {
        expect_optimal(input, ExpressionType::Value);
    }

    fn check_type(input: &str) {
        expect_optimal(input, ExpressionType::Type);
    }

    // Expects a parse error, or not-an-expression, but not a lex error
    fn expect_error(input: &str, expected_type: ExpressionType) {
        let tokens = Token::scan(input);
        let mut tokens = TokenIter::new(tokens);
        let res = Expression::parse(
            &mut tokens,
            expected_type,
            Terminator::Is(TokenType::NewLine),
        );
        match res {
            Err(_) => {}
            Ok((e, _)) => panic!("unexpectedly parsed {} => {}", input, e),
        }
    }

    // Expects the input to be an application at the top level
    fn expect_application(input: &str) {
        let exp = Expression::expect_parse(input, ExpressionType::Value);
        if let Expression::Apply(_, _) = exp {
            // That's what we expect
            return;
        }
        exp.print_one_level();
        panic!("expected a top-level apply");
    }

    fn expect_dot(input: &str) {
        let exp = Expression::expect_parse(input, ExpressionType::Value);
        if let Expression::Binary(_, token, _) = &exp {
            if token.token_type == TokenType::Dot {
                // That's what we expect
                return;
            }
        }
        exp.print_one_level();
        panic!("expected a top-level dot");
    }

    fn check_not_value(input: &str) {
        expect_error(input, ExpressionType::Value);
    }

    fn check_not_type(input: &str) {
        expect_error(input, ExpressionType::Type);
    }

    #[test]
    fn test_value_parsing() {
        check_value("p -> (q -> p)");
        check_value("(p -> (q -> r)) -> ((p -> q) -> (p -> r))");
        check_value("(p <-> q) = ((p -> q) and (q -> p))");
        check_value("p and q <-> q and p");
        check_value("(p and q) and r <-> p and (q and r)");
        check_value("p or q <-> q or p");
        check_value("(p or q) or r <-> p or (q or r)");
    }

    #[test]
    fn test_function_application() {
        check_value("f(x)");
        check_value("foo(x, y)");
        check_value("foo(x)(y)");
        check_value("foo(x, y + z, w)");
    }

    #[test]
    fn test_quantifiers() {
        check_value("forall(x: Nat) { (suc(x) = x + 1) }");
        check_value("exists(x: Nat) { (x = 0) }");
        check_value("exists(f: (Nat, Nat) -> Nat) { (f(0, 0) = 0) }");
    }

    #[test]
    fn test_type_parsing() {
        check_type("Bool");
        check_type("Bool -> Bool");
        check_type("(Bool, Bool) -> Bool");
    }

    #[test]
    fn test_comparisons() {
        check_value("p = q");
        check_value("p != q");
        check_value("p < q");
        check_value("p <= q");
        check_value("p > q");
        check_value("p >= q");
    }

    #[test]
    fn test_blocks() {
        check_value("forall(x: Nat) { x = x }");
    }

    #[test]
    fn test_empty_blocks() {
        // Empty blocks in expressions should fail to parse, but not crash
        check_not_value("forall(x: Nat) { }");
        check_not_value("exists(x: Nat) { }");
    }

    #[test]
    fn test_block_inside_binary() {
        check_value("p -> forall(x: Nat) { x = x }");
        check_value("f(forall(x: Nat) { x = x }, forall(y: Nat) { y = y })");
    }

    #[test]
    fn test_bad_values() {
        check_not_value("+ + +");

        // Not expressions
        check_not_value("let a: int = x + 2");
        check_not_value("define (p & q) = !(p -> !q)");
        check_not_value("type Nat: axiom");

        // A math-function has to have arguments
        check_not_value("f()");

        check_not_value("axiom contraposition: (!p -> !q) -> (q -> p)");
        check_not_value("f x");

        check_not_value("forall");
        check_not_value("exists");
        check_not_value("function");
    }

    #[test]
    fn test_bad_types() {
        check_not_type("bool, bool -> bool ->");
        check_not_type("(!p -> !q) -> (q -> p)");
    }

    #[test]
    fn test_extra_newline() {
        Expression::expect_value(
            "(1 +
            2)",
        );
    }

    #[test]
    fn test_dot_expression_values() {
        check_value("NatPair.first(NatPair.new(a, b)) = a");
        check_value("foo(x).bar");
        check_value("foo(x).bar.baz");
        check_value("(foo).bar");
        check_value("(a + b).c");
        check_value("a.b.c = Foo.bar(baz).qux");
    }

    #[test]
    fn test_dot_parsing_priority() {
        expect_application("foo.bar(baz)");
        expect_dot("foo(x).bar");
        expect_dot("foo(x).bar.baz");
        expect_dot("(foo).bar");
        expect_dot("(a + b).c");
        expect_dot("Foo.bar(baz).qux");
        expect_application("foo(bar).baz(qux)");
    }

    #[test]
    fn test_if_then_else_expressions() {
        check_value("if p { q } else { r }");
        check_value("if a = 0 { 0 } else { 1 }");
        check_value("if foo(a) { 0 } else { 1 }");
        check_value("if (a = 0) { 0 } else { 1 }");

        check_not_value("if");
        check_not_value("if p");
        check_not_value("if p { q }");
        check_not_value("else");
        check_not_value("else { r }");
        check_not_value("if p { q } else { r } else { s }");
    }

    #[test]
    fn test_bad_partials() {
        check_not_value("(1 +)");
        check_not_value("(!)");
        check_not_value("{ 1 }");
        check_not_value("forall(x: Nat)");
        check_not_value("forall(x: Nat) { x = x } { x }");
        check_not_value("1 + { 1 }");

        // A block should not be okay where we expect an expression
        check_not_value("{ x = x }");
    }

    #[test]
    fn test_match_expressions() {
        check_value("match a { Foo.bar(b) { b } Foo.qux(b) { b } }");
        check_not_value("match a { }");
    }

    #[test]
    fn test_unary_negate_expression() {
        check_value("-1");
        check_value("-a");
        check_value("-a - b");
        check_value("-(a - b)");
        check_value("a + -b");
        check_value("a - -b");
        check_value("-a * b");
        check_value("-(a * b)");
    }

    #[test]
    fn test_implies_expression() {
        check_value("a implies b");
    }

    #[test]
    fn test_type_params_in_expressions() {
        check_value("foo<T>");
        check_value("List<T>.new");
        check_value("map(add<Int>, myList)");
        check_value("is_surjective(identity<T>)");

        // check_value("foo.bar<T>");
    }
}

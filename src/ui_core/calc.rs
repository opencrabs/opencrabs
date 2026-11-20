//! The calculator Seeker becomes when the query is arithmetic.
//!
//! Spotlight answers `200/7.1` in the same field that finds apps, and the
//! owner asked Seeker for the same trick: type the expression, the answer
//! prints above every row, `Enter` copies it. The evaluator is pure and lives
//! here, beside the ranker, for the reason `palette` states: two frontends
//! must not answer the same keystrokes differently.
//!
//! No dependency, no shelling out, no `eval` crate. A recursive descent parser
//! over `f64`, and `None` means "this is not arithmetic", which is the only
//! answer the card has to understand: the query keeps its ordinary search
//! behaviour.

/// The longest expression the calculator will look at. A person types under
/// thirty characters; the ceiling exists so a pasted wall of digits is
/// rejected before anything parses it.
pub const MAX_INPUT: usize = 128;

/// How deep parentheses may nest. `((((((` is not a sum, and an unbounded
/// descent would walk the stack until it fell over.
const MAX_DEPTH: u32 = 32;

/// The printed form of a magnitude at or above this: nobody reads fifteen
/// digits, and the card keeps the answer on one line.
const SCIENTIFIC_ABOVE: f64 = 1e15;

/// The printed form below this, same reason from the other end.
const SCIENTIFIC_BELOW: f64 = 1e-9;

/// Evaluate a whole arithmetic expression typed into Seeker.
///
/// Returns the answer as it should print, or `None` when the text is not
/// arithmetic: prose, a bare number, a malformed expression, division by
/// zero, or a result that is not a finite number. `None` is never an error
/// the card shows; the query simply keeps its normal search behaviour.
pub fn evaluate(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_INPUT {
        return None;
    }
    if !looks_arithmetic(trimmed) {
        return None;
    }
    let mut parser = Parser {
        src: trimmed.as_bytes(),
        at: 0,
        depth: 0,
    };
    let value = parser.expr()?;
    parser.skip_spaces();
    // A whole expression, not a prefix of one: `2 3` and `2)` are not sums.
    if parser.at != parser.src.len() {
        return None;
    }
    if !value.is_finite() {
        return None;
    }
    Some(format_answer(value))
}

/// Whether the text is only arithmetic, with at least one operator that joins
/// two operands.
///
/// The second half is what keeps the card a launcher: `200` alone is a search
/// term, not a calculation, so it keeps its results. A leading sign does not
/// count either, or `-200` would turn a search into an answer.
fn looks_arithmetic(input: &str) -> bool {
    let mut previous: Option<char> = None;
    let mut joined = false;
    for ch in input.chars() {
        match ch {
            '0'..='9' | '.' => {}
            '+' | '-' | '*' | '/' | '%' | '^' => {
                // The operator must follow an operand, so unary signs and
                // doubled operators never satisfy this on their own.
                if matches!(previous, Some(prev) if prev.is_ascii_digit() || prev == ')' || prev == '.')
                {
                    joined = true;
                }
            }
            '(' | ')' => {}
            whitespace if whitespace.is_whitespace() => continue,
            _ => return false,
        }
        previous = Some(ch);
    }
    joined
}

struct Parser<'a> {
    src: &'a [u8],
    at: usize,
    depth: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    /// Consume `byte` if it is next, ignoring spaces before it.
    fn eat(&mut self, byte: u8) -> bool {
        self.skip_spaces();
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// `expr := term (('+' | '-') term)*`
    fn expr(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            if self.eat(b'+') {
                value += self.term()?;
            } else if self.eat(b'-') {
                value -= self.term()?;
            } else {
                return Some(value);
            }
        }
    }

    /// `term := unary (('*' | '/' | '%') unary)*`
    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            if self.eat(b'*') {
                value *= self.unary()?;
            } else if self.eat(b'/') {
                let divisor = self.unary()?;
                if divisor == 0.0 {
                    return None;
                }
                value /= divisor;
            } else if self.eat(b'%') {
                // Modulo, the meaning a developer expects from `200 % 7`.
                // Percent-of phrases are a separate feature, not this one.
                let divisor = self.unary()?;
                if divisor == 0.0 {
                    return None;
                }
                value %= divisor;
            } else {
                return Some(value);
            }
        }
    }

    /// `unary := ('-' | '+') unary | power`
    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }

    /// `power := atom ('^' unary)?`, so the operator is right associative:
    /// `2^3^2` reads as `2^(3^2)` and prints `512`.
    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.eat(b'^') {
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    /// `atom := number | '(' expr ')'`
    fn atom(&mut self) -> Option<f64> {
        self.skip_spaces();
        match self.peek()? {
            b'(' => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                self.at += 1;
                let value = self.expr()?;
                if !self.eat(b')') {
                    return None;
                }
                self.depth -= 1;
                Some(value)
            }
            b'0'..=b'9' | b'.' => self.number(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        let mut seen_point = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => self.at += 1,
                b'.' if !seen_point => {
                    seen_point = true;
                    self.at += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.at]).ok()?;
        text.parse::<f64>().ok()
    }
}

/// The answer as it prints: ten decimals kill the float noise (`0.1 + 0.2`
/// reads `0.3`), trailing zeros go, and a whole number keeps no decimal point
/// (`20 / 5` reads `4`). Magnitudes past either end print in scientific form
/// rather than dragging a line of digits across the card.
fn format_answer(value: f64) -> String {
    // Negative zero is a floating point artefact, never something a person
    // typed on purpose.
    let value = if value == 0.0 { 0.0 } else { value };
    let magnitude = value.abs();
    if magnitude >= SCIENTIFIC_ABOVE || (magnitude != 0.0 && magnitude < SCIENTIFIC_BELOW) {
        let text = format!("{value:.9e}");
        return match text.split_once('e') {
            Some((mantissa, exponent)) => {
                format!("{}e{exponent}", trim_zeros(mantissa))
            }
            None => text,
        };
    }
    trim_zeros(&format!("{value:.10}"))
}

fn trim_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    let mut out = text.to_string();
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

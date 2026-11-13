//! 表达式 Watch 的纯逻辑：解析并计算已加载变量的算术表达式，不访问硬件。

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Variable(String),
    Negate(Box<Expr>),
    Binary(Box<Expr>, char, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Symbol(char),
}

const MAX_LENGTH: usize = 256;
const MAX_DEPTH: usize = 32;

fn tokenize(source: &str) -> Result<Vec<Token>, String> {
    if source.len() > MAX_LENGTH {
        return Err(format!("表达式过长（最多 {MAX_LENGTH} 字节）"));
    }
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            if c == '0' && matches!(chars.get(i + 1), Some('x' | 'X')) {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = chars[start + 2..i].iter().collect();
                let value =
                    u64::from_str_radix(&text, 16).map_err(|_| "无效的十六进制数".to_owned())?;
                tokens.push(Token::Number(value as f64));
            } else {
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // 科学计数法的指数部分。
                if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && matches!(chars[j], '+' | '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                tokens.push(Token::Number(
                    text.parse::<f64>()
                        .map_err(|_| format!("无效的数字：{text}"))?,
                ));
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            // 变量路径允许成员访问和数组下标，如 a.b[2].c。
            let start = i;
            while i < chars.len() {
                let c = chars[i];
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                    i += 1;
                } else if c == '[' {
                    let mut j = i + 1;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j == i + 1 || chars.get(j) != Some(&']') {
                        return Err("数组下标只支持常数，如 a[2]".to_owned());
                    }
                    i = j + 1;
                } else {
                    break;
                }
            }
            tokens.push(Token::Name(chars[start..i].iter().collect()));
        } else if "+-*/(),".contains(c) {
            tokens.push(Token::Symbol(c));
            i += 1;
        } else {
            return Err(format!("不支持的字符：{c}"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn peek_symbol(&self, symbols: &str) -> Option<char> {
        match self.tokens.get(self.position) {
            Some(Token::Symbol(c)) if symbols.contains(*c) => Some(*c),
            _ => None,
        }
    }

    fn expect(&mut self, symbol: char) -> Result<(), String> {
        if self.peek_symbol(&symbol.to_string()).is_some() {
            self.position += 1;
            Ok(())
        } else {
            Err(format!("缺少“{symbol}”"))
        }
    }

    fn sum(&mut self, depth: usize) -> Result<Expr, String> {
        let mut left = self.product(depth)?;
        while let Some(op) = self.peek_symbol("+-") {
            self.position += 1;
            left = Expr::Binary(Box::new(left), op, Box::new(self.product(depth)?));
        }
        Ok(left)
    }

    fn product(&mut self, depth: usize) -> Result<Expr, String> {
        let mut left = self.unary(depth)?;
        while let Some(op) = self.peek_symbol("*/") {
            self.position += 1;
            left = Expr::Binary(Box::new(left), op, Box::new(self.unary(depth)?));
        }
        Ok(left)
    }

    fn unary(&mut self, depth: usize) -> Result<Expr, String> {
        if depth > MAX_DEPTH {
            return Err("表达式嵌套过深".to_owned());
        }
        if self.peek_symbol("-").is_some() {
            self.position += 1;
            return Ok(Expr::Negate(Box::new(self.unary(depth + 1)?)));
        }
        if self.peek_symbol("+").is_some() {
            self.position += 1;
            return self.unary(depth + 1);
        }
        self.atom(depth)
    }

    fn atom(&mut self, depth: usize) -> Result<Expr, String> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        match token {
            Some(Token::Number(value)) => Ok(Expr::Number(value)),
            Some(Token::Symbol('(')) => {
                let inner = self.sum(depth + 1)?;
                self.expect(')')?;
                Ok(inner)
            }
            Some(Token::Name(name)) => {
                if self.peek_symbol("(").is_none() {
                    return Ok(Expr::Variable(name));
                }
                self.position += 1;
                let mut arguments = Vec::new();
                if self.peek_symbol(")").is_none() {
                    loop {
                        arguments.push(self.sum(depth + 1)?);
                        if self.peek_symbol(",").is_some() {
                            self.position += 1;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(')')?;
                let expected = match name.as_str() {
                    "abs" | "sqrt" => 1,
                    "min" | "max" => 2,
                    _ => return Err(format!("不支持的函数：{name}（可用 abs/sqrt/min/max）")),
                };
                if arguments.len() != expected {
                    return Err(format!("{name} 需要 {expected} 个参数"));
                }
                Ok(Expr::Call(name, arguments))
            }
            Some(Token::Symbol(c)) => Err(format!("“{c}”的位置不对")),
            None => Err("表达式不完整".to_owned()),
        }
    }
}

pub fn parse(source: &str) -> Result<Expr, String> {
    let tokens = tokenize(source)?;
    if tokens.is_empty() {
        return Err("请输入表达式".to_owned());
    }
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    let expr = parser.sum(0)?;
    if parser.position != parser.tokens.len() {
        return Err("表达式末尾有多余内容".to_owned());
    }
    Ok(expr)
}

impl Expr {
    /// `lookup` 返回变量当前值；变量未找到或尚无采样时返回说明文字。
    pub fn eval(&self, lookup: &dyn Fn(&str) -> Result<f64, String>) -> Result<f64, String> {
        Ok(match self {
            Expr::Number(value) => *value,
            Expr::Variable(name) => lookup(name)?,
            Expr::Negate(inner) => -inner.eval(lookup)?,
            Expr::Binary(left, op, right) => {
                let (a, b) = (left.eval(lookup)?, right.eval(lookup)?);
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ => {
                        if b == 0.0 {
                            return Err("除数为 0".to_owned());
                        }
                        a / b
                    }
                }
            }
            Expr::Call(name, arguments) => {
                let a = arguments[0].eval(lookup)?;
                match name.as_str() {
                    "abs" => a.abs(),
                    "sqrt" => {
                        if a < 0.0 {
                            return Err("负数不能开方".to_owned());
                        }
                        a.sqrt()
                    }
                    "min" => a.min(arguments[1].eval(lookup)?),
                    _ => a.max(arguments[1].eval(lookup)?),
                }
            }
        })
    }

    pub fn variables(&self, output: &mut Vec<String>) {
        match self {
            Expr::Number(_) => {}
            Expr::Variable(name) => {
                if !output.contains(name) {
                    output.push(name.clone());
                }
            }
            Expr::Negate(inner) => inner.variables(output),
            Expr::Binary(left, _, right) => {
                left.variables(output);
                right.variables(output);
            }
            Expr::Call(_, arguments) => {
                for argument in arguments {
                    argument.variables(output);
                }
            }
        }
    }
}

/// 按名称解析变量：先精确匹配完整路径，再接受唯一的路径后缀。
pub fn resolve_name<'a, T: Copy>(
    name: &str,
    candidates: impl Iterator<Item = (&'a str, T)>,
) -> Result<T, String> {
    let mut suffix_matches = Vec::new();
    for (candidate, value) in candidates {
        if candidate == name {
            return Ok(value);
        }
        if candidate
            .strip_suffix(name)
            .is_some_and(|rest| rest.ends_with('.'))
        {
            suffix_matches.push(value);
        }
    }
    match suffix_matches.len() {
        0 => Err(format!("未加载变量：{name}")),
        1 => Ok(suffix_matches[0]),
        count => Err(format!("“{name}”匹配到 {count} 个变量，请写完整路径")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(source: &str) -> Result<f64, String> {
        parse(source)?.eval(&|name| match name {
            "target" => Ok(100.0),
            "axis.speed" => Ok(90.0),
            "m[1].cur" => Ok(-3.0),
            other => Err(format!("未加载变量：{other}")),
        })
    }

    #[test]
    fn evaluates_arithmetic_with_precedence_paths_and_functions() {
        assert_eq!(eval("target - axis.speed").unwrap(), 10.0);
        assert_eq!(eval("1 + 2 * 3").unwrap(), 7.0);
        assert_eq!(eval("(1 + 2) * 3").unwrap(), 9.0);
        assert_eq!(eval("-m[1].cur * 0x10").unwrap(), 48.0);
        assert_eq!(eval("abs(m[1].cur) / 1.5e0").unwrap(), 2.0);
        assert_eq!(eval("max(target, axis.speed) - min(1, 2)").unwrap(), 99.0);
        assert_eq!(eval("sqrt(.25)").unwrap(), 0.5);
        assert_eq!(eval("2 - -3").unwrap(), 5.0);
    }

    #[test]
    fn reports_errors_instead_of_guessing() {
        for source in [
            "", "1 +", "(1", "1 2", "a[b]", "a $ b", "pow(1,2)", "abs(1,2)", "missing",
        ] {
            assert!(eval(source).is_err(), "{source} 应当报错");
        }
        assert_eq!(eval("1 / (target - 100)").unwrap_err(), "除数为 0");
        assert!(parse(&"(".repeat(200)).is_err());
        assert!(parse(&"1+".repeat(200)).is_err());
    }

    #[test]
    fn collects_variables_once() {
        let mut names = Vec::new();
        parse("a.b + a.b * c[0]").unwrap().variables(&mut names);
        assert_eq!(names, ["a.b", "c[0]"]);
    }

    #[test]
    fn resolves_exact_name_before_unique_suffix_and_rejects_ambiguity() {
        let names = ["g.yaw.speed", "g.loader.speed", "speed", "g.yaw.kp"];
        let candidates = || names.iter().enumerate().map(|(i, n)| (*n, i));
        assert_eq!(resolve_name("speed", candidates()), Ok(2));
        assert_eq!(resolve_name("yaw.kp", candidates()), Ok(3));
        assert_eq!(resolve_name("kp", candidates()), Ok(3));
        assert!(resolve_name("loader", candidates()).is_err());
        let ambiguous = ["a.speed", "b.speed"];
        assert!(
            resolve_name("speed", ambiguous.iter().map(|n| (*n, 0)))
                .unwrap_err()
                .contains("2 个")
        );
    }
}

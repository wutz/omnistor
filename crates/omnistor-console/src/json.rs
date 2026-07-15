//! 极简 JSON 构造/解析（纯 std，够控台 API 使用）。

use std::collections::HashMap;
use std::fmt::Write;

/// JSON 值构造器（仅输出，用于响应）。
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn num(n: impl Into<f64>) -> Json {
        Json::Num(n.into())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => {
                let _ = write!(out, "{b}");
            }
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9e15 {
                    let _ = write!(out, "{}", *n as i64);
                } else {
                    let _ = write!(out, "{n}");
                }
            }
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// 解析请求体：只支持扁平对象 {"k": "v" | number | null}，控台请求足够。
pub fn parse_flat(body: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
    let mut chars = inner.chars().peekable();
    loop {
        // 找 key
        while matches!(chars.peek(), Some(c) if *c != '"') {
            if chars.next().is_none() {
                break;
            }
        }
        if chars.next() != Some('"') {
            break;
        }
        let mut key = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            key.push(c);
        }
        // 跳到冒号后的值
        while matches!(chars.peek(), Some(c) if *c == ':' || c.is_whitespace()) {
            chars.next();
        }
        let mut val = String::new();
        match chars.peek() {
            Some('"') => {
                chars.next();
                let mut escaped = false;
                for c in chars.by_ref() {
                    if escaped {
                        val.push(c);
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    } else {
                        val.push(c);
                    }
                }
            }
            Some(_) => {
                while matches!(chars.peek(), Some(c) if *c != ',' && *c != '}') {
                    val.push(chars.next().unwrap());
                }
                val = val.trim().to_string();
            }
            None => break,
        }
        if !key.is_empty() {
            map.insert(key, val);
        }
        // 跳过分隔逗号
        while matches!(chars.peek(), Some(c) if *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_escapes_and_nests() {
        let j = Json::Obj(vec![
            ("name".into(), Json::str("a\"b")),
            ("n".into(), Json::num(42u32)),
            ("arr".into(), Json::Arr(vec![Json::Bool(true), Json::Null])),
        ]);
        assert_eq!(j.render(), r#"{"name":"a\"b","n":42,"arr":[true,null]}"#);
    }

    #[test]
    fn parse_flat_object() {
        let m = parse_flat(r#"{"name": "acme", "capacity": 1000, "note": "a,b}c"}"#);
        assert_eq!(m["name"], "acme");
        assert_eq!(m["capacity"], "1000");
        assert_eq!(m["note"], "a,b}c");
    }
}

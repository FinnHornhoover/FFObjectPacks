use crate::value::{Fields, array, content, map};
use anyhow::Result;
use rmpv::Value;
use std::{collections::BTreeMap, path::Path};

struct Token<'a> {
    text: &'a str,
    line: usize,
    quoted: bool,
}

fn tokens(source: &str) -> Vec<Token<'_>> {
    let bytes = source.as_bytes();
    let (mut cursor, mut line) = (0, 1);
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\n' => {
                line += 1;
                cursor += 1;
            }
            byte if byte.is_ascii_whitespace() => cursor += 1,
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor += 2;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    if bytes[cursor] == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                        cursor += 2;
                        break;
                    }
                    cursor += 1;
                }
            }
            b'"' => {
                let token_line = line;
                cursor += 1;
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor] != b'"' {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    if bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
                        cursor += 1;
                    }
                    cursor += 1;
                }
                tokens.push(Token {
                    text: &source[start..cursor],
                    line: token_line,
                    quoted: true,
                });
                cursor = (cursor + 1).min(bytes.len());
            }
            byte => {
                let start = cursor;
                cursor += 1;
                if !b"{}()[]=,".contains(&byte) {
                    while cursor < bytes.len()
                        && !bytes[cursor].is_ascii_whitespace()
                        && !b"{}()[]=,\"".contains(&bytes[cursor])
                        && !(bytes[cursor] == b'/'
                            && matches!(bytes.get(cursor + 1), Some(b'/' | b'*')))
                    {
                        cursor += 1;
                    }
                }
                tokens.push(Token {
                    text: &source[start..cursor],
                    line,
                    quoted: false,
                });
            }
        }
    }
    tokens
}

fn declarations(tokens: &[Token<'_>]) -> Vec<Value> {
    let kinds = [
        "Shader",
        "Category",
        "SubShader",
        "Pass",
        "GrabPass",
        "Properties",
        "Tags",
        "Program",
        "SubProgram",
        "BindChannels",
        "Stencil",
        "Fog",
        "Material",
        "SetTexture",
    ];
    let commands = [
        "Pass",
        "Fog",
        "Blend",
        "BlendOp",
        "Cull",
        "ZWrite",
        "ZTest",
        "ColorMask",
        "Offset",
        "AlphaTest",
        "AlphaToMask",
        "Lighting",
        "SeparateSpecular",
        "LOD",
        "Name",
        "UsePass",
        "Fallback",
        "Comp",
        "CompFront",
        "CompBack",
        "Ref",
        "ReadMask",
        "WriteMask",
        "Fail",
        "ZFail",
        "PassFront",
        "PassBack",
        "FailFront",
        "FailBack",
        "ZFailFront",
        "ZFailBack",
        "Mode",
        "Color",
        "Density",
        "Range",
        "Ambient",
        "Diffuse",
        "Specular",
        "Shininess",
        "Emission",
    ];
    let mut headers = BTreeMap::new();
    for (i, token) in tokens.iter().enumerate() {
        let Some(kind) = kinds
            .iter()
            .find(|kind| token.text.eq_ignore_ascii_case(kind))
            .filter(|_| !token.quoted)
        else {
            continue;
        };
        for (j, next) in tokens.iter().enumerate().skip(i + 1).take(5) {
            if next.text == "{" && !next.quoted {
                headers.insert(j, *kind);
                break;
            }
            if !next.quoted
                && (next.text == "}"
                    || kinds
                        .iter()
                        .chain(&commands)
                        .any(|word| next.text.eq_ignore_ascii_case(word)))
            {
                break;
            }
        }
    }

    let mut scopes = Vec::new();
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut result = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.quoted {
            continue;
        }
        if token.text == "{" {
            let kind = headers.get(&i).copied().unwrap_or("block");
            let index = counts.entry(kind).or_default();
            scopes.push((kind, *index));
            *index += 1;
            continue;
        }
        if token.text == "}" {
            scopes.pop();
            continue;
        }
        if scopes.iter().any(|(kind, _)| {
            [
                "Program",
                "SubProgram",
                "Properties",
                "BindChannels",
                "SetTexture",
            ]
            .contains(kind)
        }) {
            continue;
        }
        let scope = array(
            scopes
                .iter()
                .map(|(kind, index)| array::<Value>([(*kind).into(), (*index).into()])),
        );
        if token.text == "=" && scopes.last().is_some_and(|(kind, _)| *kind == "Tags") && i > 0 {
            if let Some(value) = tokens.get(i + 1) {
                result.push(map([
                    ("scope", scope),
                    ("command", "Tag".into()),
                    ("arguments", array([tokens[i - 1].text, value.text])),
                    ("line", token.line.into()),
                ]));
            }
        } else if let Some(command) = commands
            .iter()
            .find(|word| token.text.eq_ignore_ascii_case(word))
            && tokens.get(i + 1).is_none_or(|next| next.text != "{")
        {
            let arguments = tokens[i + 1..].iter().take_while(|next| {
                next.line == token.line
                    && (next.quoted
                        || (!b"{}".contains(&next.text.as_bytes()[0])
                            && !commands
                                .iter()
                                .chain(&kinds)
                                .any(|word| next.text.eq_ignore_ascii_case(word))))
            });
            result.push(map([
                ("scope", scope),
                ("command", (*command).into()),
                ("arguments", array(arguments.map(|argument| argument.text))),
                ("line", token.line.into()),
            ]));
        }
    }
    result
}

pub fn describe(raw: &Value, output: &Path) -> Result<Value> {
    let script = raw["m_Script"].text();
    let tokens = tokens(script);
    let declared_name = tokens
        .windows(2)
        .find(|pair| {
            !pair[0].quoted && pair[0].text.eq_ignore_ascii_case("Shader") && pair[1].quoted
        })
        .map(|pair| pair[1].text)
        .unwrap_or("");
    let name = if raw["m_Name"].text().is_empty() {
        declared_name
    } else {
        raw["m_Name"].text()
    };
    let source = if script.is_empty() {
        Value::Nil
    } else {
        content(output, "models/shaders", script.as_bytes(), ".shaderlab")?.into()
    };
    Ok(map([
        ("name", name.into()),
        ("declared_name", declared_name.into()),
        ("path", raw["m_PathName"].clone()),
        ("source", source),
        ("default_properties", raw["m_DefaultProperties"].clone()),
        ("static_properties", raw["m_StaticProperties"].clone()),
        ("declarations", array(declarations(&tokens))),
    ]))
}

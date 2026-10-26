use crate::value::{array, map};
use anyhow::Result;
use dnfile::{
    ClrData, DnPe,
    lang::cil::instruction::{Instruction, Operand},
    stream::meta_data_tables::mdtables::{Field, MemberRef, MethodDef, TypeRef},
};
use rmpv::Value;
use sha2::{Digest, Sha256};

fn member<'a>(net: &'a ClrData<'_>, instruction: &Instruction) -> Option<&'a str> {
    let Operand::Token(token) = &instruction.operand else {
        return None;
    };
    let index = token.rid().checked_sub(1)?;
    match token.table() {
        1 => Some(
            &net.md_table("TypeRef")
                .ok()?
                .row::<TypeRef>(index)
                .ok()?
                .type_name,
        ),
        4 => Some(&net.md_table("Field").ok()?.row::<Field>(index).ok()?.name),
        6 => Some(
            &net.md_table("MethodDef")
                .ok()?
                .row::<MethodDef>(index)
                .ok()?
                .name,
        ),
        10 => Some(
            &net.md_table("MemberRef")
                .ok()?
                .row::<MemberRef>(index)
                .ok()?
                .name,
        ),
        _ => None,
    }
}

fn string(net: &ClrData<'_>, instruction: &Instruction) -> Option<String> {
    let Operand::StringToken(token) = &instruction.operand else {
        return None;
    };
    net.get_us(token.rid()).ok()
}

fn colour(net: &ClrData<'_>, instructions: &[Instruction]) -> Option<(Value, usize)> {
    let first = instructions.first()?;
    if first.is_ldstr() && member(net, instructions.get(1)?) == Some("FromColorCode") {
        let code = string(net, first)?;
        let code = code.strip_prefix('#').unwrap_or(&code);
        if code.len() != 6 || !code.is_ascii() {
            return None;
        }
        let channels = (0..3)
            .map(|i| {
                u8::from_str_radix(&code[i * 2..i * 2 + 2], 16)
                    .ok()
                    .map(|n| Value::F32(n as f32 / 255.0))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut channels = channels;
        channels.push(Value::F32(1.0));
        return Some((array(channels), 2));
    }

    let mut cursor = usize::from(first.opcode.name == "ldelema");
    let mut channels = Vec::new();
    while let Some(value) = instructions.get(cursor).and_then(Instruction::get_ldc) {
        channels.push(Value::F32(value as f32));
        cursor += 1;
        if channels.len() == 4 {
            break;
        }
    }
    if !(3..=4).contains(&channels.len()) || member(net, instructions.get(cursor)?) != Some(".ctor")
    {
        return None;
    }
    if channels.len() == 3 {
        channels.push(Value::F32(1.0));
    }
    Some((array(channels), cursor + 1))
}

fn eye_swatches(net: &ClrData<'_>, instructions: &[Instruction]) -> Option<Vec<Value>> {
    let mut constructor = None;
    let mut applied = None;
    let mut swatches = std::collections::BTreeMap::new();
    for (i, instruction) in instructions.iter().enumerate() {
        match member(net, instruction) {
            Some(".ctor") => {
                let Operand::Token(token) = &instruction.operand else {
                    continue;
                };
                if token.table() != 10 {
                    continue;
                }
                let reference = net
                    .md_table("MemberRef")
                    .ok()?
                    .row::<MemberRef>(token.rid().checked_sub(1)?)
                    .ok()?;
                let owner = net.resolve_coded_index::<TypeRef>(&reference.class).ok();
                if !owner.is_some_and(|owner| owner.type_name == "Color") {
                    continue;
                }
                let start = instructions[..i]
                    .iter()
                    .rposition(|ins| !ins.is_ldc())
                    .map(|j| j + 1)
                    .unwrap_or(0);
                let channels = instructions[start..i]
                    .iter()
                    .map(Instruction::get_ldc)
                    .collect::<Option<Vec<_>>>()?;
                constructor = if (3..=4).contains(&channels.len()) {
                    let mut channels: Vec<Value> =
                        channels.into_iter().map(|n| Value::F32(n as f32)).collect();
                    if channels.len() == 3 {
                        channels.push(Value::F32(1.0));
                    }
                    Some(array(channels))
                } else {
                    None
                };
            }
            Some("set_color") => applied = constructor.take(),
            Some("EyeColorSelected") => {
                let index = i.checked_sub(1).and_then(|i| instructions[i].get_ldc())?;
                if index < 0.0 || index > 1024.0 || index.fract() != 0.0 {
                    return None;
                }
                let colour = applied.clone()?;
                if swatches
                    .insert(index as usize, colour.clone())
                    .is_some_and(|old| old != colour)
                {
                    return None;
                }
            }
            _ => {}
        }
    }
    if swatches.is_empty() || swatches.keys().copied().ne(0..swatches.len()) {
        return None;
    }
    Some(swatches.into_values().collect())
}

pub fn palettes(data: &[u8], assembly: &str, bundle: &str) -> Result<Vec<Value>> {
    let pe = DnPe::parse(data)?;
    let net = pe.net()?;
    let mut records = Vec::new();
    let sha256 = hex::encode(Sha256::digest(data));

    for function in net.functions() {
        let instructions = &function.instructions;
        if let Some(values) = eye_swatches(net, instructions) {
            records.push(map([
                ("assembly", assembly.into()),
                ("bundle", bundle.into()),
                ("sha256", sha256.clone().into()),
                ("method_offset", function.offset.into()),
                ("field", "eyeSwatches".into()),
                ("supported", true.into()),
                ("selection_index_base", 0.into()),
                ("style_index_base", 1.into()),
                ("values", array(values)),
            ]));
        }
        for (i, instruction) in instructions.iter().enumerate() {
            if !["stfld", "stsfld"].contains(&instruction.opcode.name) {
                continue;
            }
            let Some(field) = member(net, instruction) else {
                continue;
            };
            if field == "szTextureSuffix" {
                if let Some(value) = i.checked_sub(1).and_then(|i| string(net, &instructions[i])) {
                    records.push(map([
                        ("assembly", assembly.into()),
                        ("bundle", bundle.into()),
                        ("sha256", sha256.clone().into()),
                        ("method_offset", function.offset.into()),
                        ("field", field.into()),
                        ("values", value.into()),
                    ]));
                }
                continue;
            }
            if !["skinColor", "hairColor", "eyeColor", "uniqueEyes"].contains(&field) {
                continue;
            }
            let start = instructions[..i]
                .iter()
                .rposition(|ins| ins.opcode.name == "newarr");
            let Some(start) = start.filter(|start| *start > 0) else {
                continue;
            };
            let Some(count) = instructions[start - 1]
                .get_ldc()
                .filter(|n| *n > 0.0 && *n <= 1024.0 && n.fract() == 0.0)
            else {
                continue;
            };
            let expected_type = if field == "uniqueEyes" {
                "Int32"
            } else {
                "Color"
            };
            if member(net, &instructions[start]) != Some(expected_type) {
                continue;
            }
            let mut values = vec![None; count as usize];
            let mut cursor = start + 1;
            let mut supported = true;
            while cursor < i {
                if instructions[cursor].opcode.name != "dup" {
                    supported = false;
                    break;
                }
                let Some(index) = instructions
                    .get(cursor + 1)
                    .and_then(Instruction::get_ldc)
                    .filter(|index| *index >= 0.0 && *index < count && index.fract() == 0.0)
                else {
                    supported = false;
                    break;
                };
                cursor += 2;
                if field == "uniqueEyes" {
                    let Some(value) = instructions.get(cursor).and_then(Instruction::get_ldc)
                    else {
                        supported = false;
                        break;
                    };
                    values[index as usize] = Some((value as i64).into());
                    cursor += 1;
                } else if let Some((value, consumed)) = colour(net, &instructions[cursor..i]) {
                    values[index as usize] = Some(value);
                    cursor += consumed;
                } else {
                    supported = false;
                    break;
                }

                if cursor < i && instructions[cursor].opcode.name.starts_with("stelem") {
                    cursor += 1;
                }
            }
            let values = values.into_iter().collect::<Option<Vec<_>>>();
            records.push(map([
                ("assembly", assembly.into()),
                ("bundle", bundle.into()),
                ("sha256", sha256.clone().into()),
                ("method_offset", function.offset.into()),
                ("field", field.into()),
                ("supported", (supported && values.is_some()).into()),
                (
                    "values",
                    if supported {
                        values.map(array).unwrap_or(Value::Nil)
                    } else {
                        Value::Nil
                    },
                ),
            ]));
        }
    }
    Ok(records)
}

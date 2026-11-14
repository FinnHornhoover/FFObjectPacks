use crate::{
    scan::Scan,
    unity::tree::Tree,
    value::{Fields, array, content, map, rawref, serial, write_json, write_pack},
};
use anyhow::{Context, Result, ensure};
use rmpv::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn classify(tree: &Tree) -> Option<&'static str> {
    let has = |name: &str| tree.children.iter().any(|field| field.name == name);
    if has("musicDatas") && has("ambientDatas") {
        Some("MusicDataStorage")
    } else if has("EvSoundArray") {
        Some("EnviroSoundScript")
    } else if has("musicDataStorage") {
        Some("MusicController")
    } else {
        None
    }
}

fn crc(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0; 256];
        let mut i = 0;
        while i < 256 {
            let mut value = (i as u32) << 24;
            let mut bit = 0;
            while bit < 8 {
                value = (value << 1)
                    ^ if value & 0x8000_0000 != 0 {
                        0x04c1_1db7
                    } else {
                        0
                    };
                bit += 1;
            }
            table[i] = value;
            i += 1;
        }
        table
    };

    bytes.iter().fold(0u32, |value, byte| {
        (value << 8) ^ TABLE[((value >> 24) as u8 ^ byte) as usize]
    })
}

pub fn normalize_ogg(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut output = bytes.to_vec();
    let mut streams = BTreeMap::new();
    let mut offset = 0;
    while offset < output.len() {
        let header = output
            .get(offset..offset + 27)
            .context("Truncated Ogg page")?;
        ensure!(
            &header[..4] == b"OggS" && header[4] == 0,
            "Invalid Ogg page"
        );
        let count = header[26] as usize;
        let segments = output
            .get(offset + 27..offset + 27 + count)
            .context("Truncated Ogg segment table")?;
        let size = 27 + count + segments.iter().map(|v| *v as usize).sum::<usize>();
        let page = output
            .get_mut(offset..offset + size)
            .context("Truncated Ogg payload")?;
        let original = u32::from_le_bytes(page[14..18].try_into()?);
        let next = u32::try_from(streams.len() + 1)?;
        let serial = *streams.entry(original).or_insert(next);
        page[14..18].copy_from_slice(&serial.to_le_bytes());
        page[22..26].fill(0);
        let checksum = crc(page);
        page[22..26].copy_from_slice(&checksum.to_le_bytes());
        offset += size;
    }
    ensure!(offset > 0, "Empty Ogg stream");
    Ok(output)
}

pub fn describe(raw: &Value, output: &Path) -> Result<Value> {
    let bytes = raw["audio data"].binary();
    let mut descriptor = raw.without(&["audio data"]);
    descriptor.set("name", raw["m_Name"].clone());
    descriptor.set("source_bytes", bytes.len().into());
    descriptor.set("file", Value::Nil);
    descriptor.set("supported", false.into());

    if bytes.is_empty() {
        descriptor.set("error", "Empty or externally stored audio payload".into());
        return Ok(descriptor);
    }

    let (payload, extension, mime, encoding, error) = if bytes.starts_with(b"OggS") {
        match normalize_ogg(bytes) {
            Ok(data) => {
                let start = 27 + bytes[26] as usize;
                let packet = &bytes[start..];
                let encoding = if packet.starts_with(b"\x01vorbis") {
                    "vorbis"
                } else if packet.starts_with(b"OpusHead") {
                    "opus"
                } else {
                    "unknown"
                };
                let error = (encoding == "unknown").then(|| "Unsupported Ogg codec".to_owned());
                (data, ".ogg", "audio/ogg", encoding, error)
            }
            Err(error) => (
                bytes.to_vec(),
                ".bin",
                "application/octet-stream",
                "unknown",
                Some(error.to_string()),
            ),
        }
    } else {
        (
            bytes.to_vec(),
            ".bin",
            "application/octet-stream",
            "unknown",
            Some("Unsupported audio container".into()),
        )
    };

    descriptor.set(
        "file",
        content(output, "audio/clips", &payload, extension)?.into(),
    );
    descriptor.set("bytes", payload.len().into());
    descriptor.set("mime", mime.into());
    descriptor.set("encoding", encoding.into());
    descriptor.set("supported", error.is_none().into());
    if let Some(error) = error {
        descriptor.set("error", error.into());
    }
    Ok(descriptor)
}

fn path(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

fn sound_path(namespace: &str, name: &str) -> Option<String> {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty()
        || ["none", "none.ogg", "null"].contains(&lower.as_str())
        || lower.contains("...")
    {
        None
    } else {
        Some(path(&format!("{namespace}/{name}")))
    }
}

pub fn events(source: &Value) -> Value {
    array(source.array().iter().map(|event| {
        let mut event = event.clone();
        if event["functionName"].text() == "sound" {
            let reference = sound_path("sound", event["data"].text());
            event.set("audio", reference.map(Value::from).unwrap_or(Value::Nil));
        }
        event
    }))
}

fn variants(pattern: &str) -> Result<Vec<String>> {
    let mut result = vec![pattern.to_owned()];
    loop {
        let mut expanded = Vec::new();
        let mut changed = false;
        for value in result {
            let Some(start) = value.find("(rand:") else {
                expanded.push(value);
                continue;
            };
            changed = true;
            let end = value[start..]
                .find(')')
                .context("Unclosed RAND expression")?
                + start;
            let range = &value[start + 6..end];
            let (first, last) = range.split_once('-').context("Invalid RAND range")?;
            let first: u32 = first.parse().context("Invalid RAND start")?;
            let last: u32 = last.parse().context("Invalid RAND end")?;
            ensure!(
                last >= first && u64::from(last) - u64::from(first) < 256,
                "Invalid or excessive RAND range"
            );
            for number in first..=last {
                ensure!(expanded.len() < 256, "RAND expression exceeds 256 variants");
                expanded.push(format!("{}{number}{}", &value[..start], &value[end + 1..]));
            }
        }
        ensure!(
            expanded.len() <= 256,
            "RAND expression exceeds 256 variants"
        );
        if !changed {
            return Ok(expanded);
        }
        result = expanded;
    }
}

struct Resolver {
    paths: BTreeMap<String, BTreeSet<u32>>,
    requests: BTreeMap<String, Value>,
}

impl Resolver {
    fn request(&mut self, key: &str) {
        if self.requests.contains_key(key) {
            return;
        }
        let data = match variants(key) {
            Ok(names) => map([(
                "variants",
                array(names.into_iter().map(|name| {
                    let ids = self.paths.get(&name).cloned().unwrap_or_default();
                    map([("path", name.into()), ("objects", array(ids))])
                })),
            )]),
            Err(error) => map([("error", error.to_string().into())]),
        };
        self.requests.insert(key.into(), data);
    }

    fn annotate(&mut self, row: &mut Value, namespace: &str, field: &str) {
        let key = sound_path(namespace, row[field].text());
        if let Some(key) = &key {
            self.request(key);
        }
        row.set("audio", key.map(Value::from).unwrap_or(Value::Nil));
    }
}

pub fn export(scan: &Scan, output: &Path) -> Result<Value> {
    let mut aliases: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let mut paths: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for (name, id) in &scan.prefabs {
        if let Some(id) = id.filter(|id| scan.node(*id).is_some_and(|node| node.kind == 83)) {
            aliases.entry(id).or_default().insert(name.clone());
            paths.entry(path(name)).or_default().insert(id);
        }
    }

    let mut resolver = Resolver {
        paths,
        requests: BTreeMap::new(),
    };
    let mut clips = BTreeMap::new();
    let mut event_clips = BTreeMap::new();
    let mut world = BTreeMap::new();
    let mut files = BTreeSet::new();
    let mut unsupported = Vec::new();
    let mut unaliased = Vec::new();
    let mut missing_clips = Vec::new();
    let mut event_count = 0usize;
    let mut emitter_count = 0usize;
    let mut region_count = 0usize;

    for (id, node) in scan
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| node.as_ref().map(|n| (id as u32, n)))
    {
        if node.kind == 83 {
            let mut data = node.data.clone();
            data.set("asset", node.asset.into());
            data.set(
                "source_paths",
                array(aliases.get(&id).cloned().unwrap_or_default()),
            );
            if data["source_paths"].array().is_empty() {
                unaliased.push(id);
            }
            if let Some(file) = data["file"].as_str() {
                files.insert(file.to_owned());
            }
            if !data["supported"].truth() {
                unsupported.push(map([
                    ("object", id.into()),
                    ("error", data["error"].clone()),
                ]));
            }
            clips.insert(id, data);
        } else if node.kind == 74 {
            let events = &node.data["events"];
            if events.array().is_empty() {
                continue;
            }
            event_count += events.array().len();
            for event in events.array() {
                if let Some(key) = event["audio"].as_str() {
                    resolver.request(key);
                }
            }
            event_clips.insert(id, events.clone());
        } else if node.kind == 82 || !node.data["audio_type"].is_nil() {
            let kind = if node.kind == 82 {
                "AudioSource"
            } else {
                node.data["audio_type"].text()
            };
            let mut data = node.data.clone();
            for (field, namespace, filename) in [
                ("musicDatas", "bgm", "fileName"),
                ("ambientDatas", "bgm", "fileName"),
                ("EvSoundArray", "enviro sound", "FileName"),
            ] {
                if data[field].is_nil() {
                    continue;
                }
                let mut rows = data[field].array().to_vec();
                for row in &mut rows {
                    resolver.annotate(row, namespace, filename);
                }
                if field == "EvSoundArray" {
                    emitter_count += rows.len();
                } else {
                    region_count += rows.len();
                }
                data.set(field, array(rows));
            }
            if node.kind == 82
                && let Some(clip) = rawref(&data, "m_audioClip")
                && !scan.node(clip).is_some_and(|node| node.kind == 83)
            {
                missing_clips.push(map([("source", id.into()), ("clip", clip.into())]));
            }
            world.insert(
                id,
                map([
                    ("type", kind.into()),
                    ("asset", node.asset.into()),
                    ("data", data),
                ]),
            );
        }

        if node.data["world_type"].text() == "BulletTableScript" {
            for bullet in node.data["m_pBulletData"].array() {
                let name = bullet["m_strSuccSound"].text();
                if !name.contains('"') && sound_path("sound", name).is_some() {
                    resolver.request(&path(&format!("sound/{name}.wav")));
                }
            }
        }
    }

    let mut missing = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    let mut invalid = Vec::new();
    for (pattern, request) in &resolver.requests {
        if !request["error"].is_nil() {
            invalid.push(map([
                ("path", pattern.clone().into()),
                ("error", request["error"].clone()),
            ]));
        }
        for variant in request["variants"].array() {
            let name = variant["path"].text().to_owned();
            if variant["objects"].array().is_empty() {
                missing.insert(name);
            } else if variant["objects"].array().len() > 1 {
                ambiguous.insert(name);
            }
        }
    }

    write_pack(
        &output.join("audio/clips.msgpack"),
        &map([
            ("clips", serial(&clips)),
            ("paths", serial(&resolver.paths)),
        ]),
    )?;
    write_pack(&output.join("audio/events.msgpack"), &serial(&event_clips))?;
    write_pack(&output.join("audio/world.msgpack"), &serial(&world))?;
    write_pack(
        &output.join("audio/references.msgpack"),
        &serial(&resolver.requests),
    )?;
    write_json(
        &output.join("audio/index.json"),
        &map([
            ("clips", "audio/clips.msgpack".into()),
            ("events", "audio/events.msgpack".into()),
            ("world", "audio/world.msgpack".into()),
            ("references", "audio/references.msgpack".into()),
        ]),
    )?;

    Ok(map([
        ("clips", clips.len().into()),
        ("files", files.len().into()),
        ("events", event_count.into()),
        ("regions", region_count.into()),
        ("emitters", emitter_count.into()),
        ("unsupported", array(unsupported)),
        ("unaliased", array(unaliased)),
        ("missing_clip_references", array(missing_clips)),
        ("missing_paths", array(missing)),
        ("ambiguous_paths", array(ambiguous)),
        ("invalid_patterns", array(invalid)),
        (
            "scan_errors",
            array(
                scan.errors
                    .iter()
                    .filter(|error| {
                        [82, 83].contains(&error["type"].integer()) || !error["audio_type"].is_nil()
                    })
                    .cloned(),
            ),
        ),
    ]))
}

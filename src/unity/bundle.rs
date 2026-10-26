use super::reader::Reader;
use anyhow::{Result, bail, ensure};
use std::{fs, io::Read, path::Path};

pub struct Member {
    pub name: String,
    pub data: Vec<u8>,
    pub version: String,
}

pub fn read(path: &Path) -> Result<Option<Vec<Member>>> {
    let bytes = fs::read(path)?;
    if !bytes.starts_with(b"Unity") && !bytes.starts_with(b"streamed\0") {
        return Ok(None);
    }

    let mut r = Reader::new(&bytes, true);
    let signature = r.cstr()?;
    ensure!(
        ["UnityWeb", "UnityRaw", "streamed"].contains(&signature.as_str()),
        "Unsupported bundle signature {signature}"
    );
    r.u32()?;
    r.cstr()?;
    let version = r.cstr()?;
    r.u32()?;
    let header = r.u32()? as usize;
    ensure!(header > 0 && header <= bytes.len(), "Invalid bundle header");

    let payload = if signature == "UnityRaw" {
        bytes[header..].to_vec()
    } else {
        let stream = xz2::stream::Stream::new_lzma_decoder(u64::MAX)?;
        let mut decoder = xz2::read::XzDecoder::new_stream(&bytes[header..], stream);
        let mut result = Vec::new();
        decoder.read_to_end(&mut result)?;
        result
    };

    let mut r = Reader::new(&payload, true);
    let count = r.count()?;
    ensure!(count > 0 && count < 100000, "Invalid bundle member count");
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push((r.cstr()?, r.u32()? as usize, r.u32()? as usize));
    }

    let mut members = Vec::new();
    for (name, offset, size) in entries {
        if offset < r.pos
            || offset
                .checked_add(size)
                .is_none_or(|end| end > payload.len())
        {
            bail!("Invalid bundle member {name}");
        }
        members.push(Member {
            name,
            data: payload[offset..offset + size].to_vec(),
            version: version.clone(),
        });
    }

    Ok(Some(members))
}

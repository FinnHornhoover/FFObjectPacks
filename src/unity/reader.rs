use anyhow::{Result, ensure};

pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub big: bool,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], big: bool) -> Self {
        Self { data, pos: 0, big }
    }

    pub fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("Reader offset overflow"))?;
        ensure!(
            end <= self.data.len(),
            "Read beyond buffer at {} + {count} / {}",
            self.pos,
            self.data.len()
        );

        let result = &self.data[self.pos..end];
        self.pos = end;
        Ok(result)
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        ensure!(pos <= self.data.len(), "Invalid offset {pos}");
        self.pos = pos;
        Ok(())
    }

    pub fn uint(&mut self, n: usize) -> Result<u64> {
        let big = self.big;
        let bytes = self.take(n)?;

        Ok(if big {
            bytes.iter().fold(0, |a, b| (a << 8) | u64::from(*b))
        } else {
            bytes.iter().rev().fold(0, |a, b| (a << 8) | u64::from(*b))
        })
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.uint(1)? as u8)
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(self.uint(4)? as u32)
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    pub fn cstr(&mut self) -> Result<String> {
        let end = self.data[self.pos..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| anyhow::anyhow!("Unterminated string"))?;
        let bytes = self.take(end)?;
        let result = String::from_utf8(bytes.to_vec())?;
        self.take(1)?;
        Ok(result)
    }

    pub fn count(&mut self) -> Result<usize> {
        let count = self.u32()? as usize;
        ensure!(count <= self.data.len(), "Invalid entry count {count}");
        Ok(count)
    }
}

//! Minimal GGUF v3 reader for the section model's weights.
//!
//! Only `GGML_TYPE_F32` tensors are supported. The file is untrusted: every
//! offset and length is checked against the real file size before slicing, and
//! every multiplication that involves file-supplied numbers is checked.

use crate::ffi::{Error, Result};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

const MAGIC: &[u8; 4] = b"GGUF";
const VERSION: u32 = 3;
const TYPE_F32: u32 = 0;
const DEFAULT_ALIGNMENT: u64 = 32;
const MAX_DIMS: u32 = 8;
/// Smallest possible tensor-directory entry: 8 (name len) + 4 (n_dims) + 4 (type) + 8 (offset).
const MIN_TENSOR_ENTRY: u64 = 24;
/// Smallest possible metadata entry: 8 (key len) + 4 (value type).
const MIN_KV_ENTRY: u64 = 12;
const MAX_ARRAY_DEPTH: u32 = 4;

struct Entry {
    range: Range<usize>,
    shape: Vec<usize>,
}

/// All tensors of one GGUF file, decoded to `f32` once at load time.
pub struct Weights {
    data: Vec<f32>,
    tensors: HashMap<String, Entry>,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: u64) -> Result<&'a [u8]> {
        let n = usize::try_from(n).map_err(|_| Error::msg("gguf: length exceeds address space"))?;
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| Error::msg(format!("gguf: read of {n} bytes at {} runs past end of file ({} bytes)", self.pos, self.buf.len())))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn remaining(&self) -> u64 {
        (self.buf.len() - self.pos) as u64
    }
    fn string(&mut self) -> Result<String> {
        let len = self.u64()?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::msg("gguf: string is not valid UTF-8"))
    }
    /// Skip one metadata value of type `ty`; returns it as `u64` when it is an integer scalar.
    fn value(&mut self, ty: u32, depth: u32) -> Result<Option<u64>> {
        let fixed = |c: &mut Self, n: u64| -> Result<Option<u64>> {
            c.take(n)?;
            Ok(None)
        };
        match ty {
            0 | 1 | 7 => fixed(self, 1),
            2 | 3 => fixed(self, 2),
            4 => Ok(Some(u64::from(self.u32()?))),
            5 | 6 => fixed(self, 4),
            10 => Ok(Some(self.u64()?)),
            11 | 12 => fixed(self, 8),
            8 => {
                self.string()?;
                Ok(None)
            }
            9 => {
                if depth >= MAX_ARRAY_DEPTH {
                    return Err(Error::msg("gguf: metadata arrays nested too deeply"));
                }
                let elem = self.u32()?;
                let count = self.u64()?;
                // Every element occupies at least one byte, which bounds the loop by file size.
                if count > self.remaining() {
                    return Err(Error::msg("gguf: metadata array longer than the file"));
                }
                for _ in 0..count {
                    self.value(elem, depth + 1)?;
                }
                Ok(None)
            }
            t => Err(Error::msg(format!("gguf: unknown metadata value type {t}"))),
        }
    }
}

impl Weights {
    pub fn load(path: &Path) -> Result<Weights> {
        let bytes = std::fs::read(path)
            .map_err(|e| Error::msg(format!("gguf: cannot read {}: {e}", path.display())))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Weights> {
        let mut c = Cursor { buf: bytes, pos: 0 };
        if c.take(4)? != MAGIC {
            return Err(Error::msg("gguf: bad magic"));
        }
        let version = c.u32()?;
        if version != VERSION {
            return Err(Error::msg(format!("gguf: unsupported version {version} (want {VERSION})")));
        }
        let n_tensors = c.u64()?;
        let n_kv = c.u64()?;
        if n_tensors.checked_mul(MIN_TENSOR_ENTRY).is_none_or(|b| b > c.remaining()) {
            return Err(Error::msg("gguf: tensor count larger than the file can hold"));
        }
        if n_kv.checked_mul(MIN_KV_ENTRY).is_none_or(|b| b > c.remaining()) {
            return Err(Error::msg("gguf: metadata count larger than the file can hold"));
        }

        let mut alignment = DEFAULT_ALIGNMENT;
        for _ in 0..n_kv {
            let key = c.string()?;
            let ty = c.u32()?;
            let v = c.value(ty, 0)?;
            if key == "general.alignment" {
                alignment = v.ok_or_else(|| Error::msg("gguf: general.alignment is not an integer"))?;
                if alignment == 0 || !alignment.is_power_of_two() {
                    return Err(Error::msg(format!("gguf: invalid alignment {alignment}")));
                }
            }
        }

        struct Dir {
            name: String,
            shape: Vec<usize>,
            offset: u64,
            bytes: u64,
        }
        let mut dir = Vec::with_capacity(n_tensors as usize);
        for _ in 0..n_tensors {
            let name = c.string()?;
            let n_dims = c.u32()?;
            if n_dims == 0 || n_dims > MAX_DIMS {
                return Err(Error::msg(format!("gguf: tensor {name}: {n_dims} dims is out of range")));
            }
            let mut shape = Vec::with_capacity(n_dims as usize);
            let mut elems: u64 = 1;
            for _ in 0..n_dims {
                let d = c.u64()?;
                elems = elems
                    .checked_mul(d)
                    .ok_or_else(|| Error::msg(format!("gguf: tensor {name}: element count overflows")))?;
                shape.push(usize::try_from(d).map_err(|_| Error::msg(format!("gguf: tensor {name}: dimension too large")))?);
            }
            // GGUF lists the fastest-varying dimension first; expose numpy (row-major) order.
            shape.reverse();
            let ty = c.u32()?;
            if ty != TYPE_F32 {
                return Err(Error::msg(format!("gguf: tensor {name}: type {ty} is not F32")));
            }
            let offset = c.u64()?;
            let bytes = elems
                .checked_mul(4)
                .ok_or_else(|| Error::msg(format!("gguf: tensor {name}: byte size overflows")))?;
            dir.push(Dir { name, shape, offset, bytes });
        }

        let align = alignment;
        let data_start = (c.pos as u64)
            .checked_add(align - 1)
            .ok_or_else(|| Error::msg("gguf: data offset overflows"))?
            & !(align - 1);
        let file_len = bytes.len() as u64;
        if data_start > file_len {
            return Err(Error::msg("gguf: data section starts past end of file"));
        }

        let mut data: Vec<f32> = Vec::new();
        let mut tensors = HashMap::with_capacity(dir.len());
        for t in dir {
            if t.offset % align != 0 {
                return Err(Error::msg(format!("gguf: tensor {}: offset {} not aligned", t.name, t.offset)));
            }
            let start = data_start
                .checked_add(t.offset)
                .ok_or_else(|| Error::msg(format!("gguf: tensor {}: offset overflows", t.name)))?;
            let end = start
                .checked_add(t.bytes)
                .filter(|&e| e <= file_len)
                .ok_or_else(|| Error::msg(format!("gguf: tensor {}: data runs past end of file", t.name)))?;
            let raw = &bytes[start as usize..end as usize];
            let first = data.len();
            data.extend(raw.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())));
            let entry = Entry { range: first..data.len(), shape: t.shape };
            if tensors.insert(t.name.clone(), entry).is_some() {
                return Err(Error::msg(format!("gguf: duplicate tensor {}", t.name)));
            }
        }
        Ok(Weights { data, tensors })
    }

    pub fn tensor(&self, name: &str) -> Result<(&[f32], &[usize])> {
        let e = self
            .tensors
            .get(name)
            .ok_or_else(|| Error::msg(format!("gguf: tensor {name:?} not found")))?;
        Ok((&self.data[e.range.clone()], &e.shape))
    }

    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }
}

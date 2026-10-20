//! Minimal reader for tiled (Big)TIFF / OME-TIFF images whose tiles use a codec the
//! `tiff` crate does not decode (Xenium morphology images: JPEG 2000, compression
//! 34712). It returns raw tile bytes per pyramid level (the first IFD plus its
//! SubIFDs) for an external decoder, and the channel name from the OME-XML.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

pub const COMPRESSION_JPEG2000: u16 = 34712;

#[derive(Debug, Clone)]
pub struct TiledLevel {
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    offsets: Vec<u64>,
    counts: Vec<u64>,
}

impl TiledLevel {
    pub fn tiles_across(&self) -> u32 {
        self.width.div_ceil(self.tile_width)
    }
    pub fn tiles_down(&self) -> u32 {
        self.height.div_ceil(self.tile_height)
    }
}

#[derive(Debug)]
pub struct TiledTiff {
    pub path: PathBuf,
    file: File,
    len: u64,
    /// Level 0 = full resolution, then the SubIFD reductions in file order.
    pub levels: Vec<TiledLevel>,
    pub bits_per_sample: u16,
    pub samples_per_pixel: u16,
    pub compression: u16,
    /// TIFF SampleFormat (1 unsigned, 2 signed, 3 float).
    pub sample_format: u16,
    pub description: Option<String>,
}

struct Reader<'a> {
    f: &'a File,
    big: bool,
    le: bool,
}

impl Reader<'_> {
    fn bytes(&self, off: u64, n: usize) -> Result<Vec<u8>> {
        let mut b = vec![0u8; n];
        self.f
            .read_exact_at(&mut b, off)
            .with_context(|| format!("read {n} bytes at {off}"))?;
        Ok(b)
    }
    fn u(&self, b: &[u8]) -> u64 {
        let mut v = 0u64;
        if self.le {
            for (i, x) in b.iter().enumerate() {
                v |= (*x as u64) << (8 * i);
            }
        } else {
            for x in b {
                v = (v << 8) | *x as u64;
            }
        }
        v
    }
}

/// One IFD entry: tag, type, count and where its values live.
struct Entry {
    tag: u16,
    typ: u16,
    count: u64,
    /// Inline value bytes, or the offset of the values.
    inline: Vec<u8>,
    offset: Option<u64>,
}

fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 1,
    }
}

fn read_ifd(r: &Reader, off: u64) -> Result<(Vec<Entry>, u64)> {
    let (count_size, entry_size, value_size, next_size) =
        if r.big { (8, 20, 8, 8) } else { (2, 12, 4, 4) };
    let n = r.u(&r.bytes(off, count_size)?);
    if n > 10_000 {
        bail!("implausible IFD entry count {n}");
    }
    let raw = r.bytes(off + count_size as u64, n as usize * entry_size)?;
    let mut out = Vec::with_capacity(n as usize);
    for e in raw.chunks_exact(entry_size) {
        let tag = r.u(&e[0..2]) as u16;
        let typ = r.u(&e[2..4]) as u16;
        let (count, val) = if r.big {
            (r.u(&e[4..12]), &e[12..20])
        } else {
            (r.u(&e[4..8]), &e[8..12])
        };
        let total = count.saturating_mul(type_size(typ) as u64);
        let (inline, offset) = if total <= value_size as u64 {
            (val[..total as usize].to_vec(), None)
        } else {
            (vec![], Some(r.u(val)))
        };
        out.push(Entry {
            tag,
            typ,
            count,
            inline,
            offset,
        });
    }
    let next = r.u(&r.bytes(off + count_size as u64 + n * entry_size as u64, next_size)?);
    Ok((out, next))
}

fn values(r: &Reader, e: &Entry, max: u64) -> Result<Vec<u64>> {
    if e.count > max {
        bail!("tag {} has {} values (limit {max})", e.tag, e.count);
    }
    let size = type_size(e.typ);
    let bytes = match e.offset {
        None => e.inline.clone(),
        Some(o) => r.bytes(o, e.count as usize * size)?,
    };
    Ok(bytes.chunks_exact(size).map(|c| r.u(c)).collect())
}

fn ascii(r: &Reader, e: &Entry) -> Result<String> {
    let bytes = match e.offset {
        None => e.inline.clone(),
        Some(o) => r.bytes(o, e.count.min(64 << 20) as usize)?,
    };
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .to_string())
}

fn level_from(r: &Reader, entries: &[Entry]) -> Result<TiledLevel> {
    let get = |t: u16| entries.iter().find(|e| e.tag == t);
    let one = |t: u16| -> Result<u64> {
        let e = get(t).ok_or_else(|| anyhow!("missing TIFF tag {t}"))?;
        Ok(*values(r, e, 1 << 20)?
            .first()
            .ok_or_else(|| anyhow!("empty TIFF tag {t}"))?)
    };
    let (width, height) = (one(256)? as u32, one(257)? as u32);
    if get(322).is_none() {
        bail!("not a tiled TIFF (no TileWidth tag)");
    }
    let (tw, th) = (one(322)? as u32, one(323)? as u32);
    let n = (width.div_ceil(tw.max(1)) as u64) * (height.div_ceil(th.max(1)) as u64);
    let offsets = values(
        r,
        get(324).ok_or_else(|| anyhow!("missing TileOffsets"))?,
        16 << 20,
    )?;
    let counts = values(
        r,
        get(325).ok_or_else(|| anyhow!("missing TileByteCounts"))?,
        16 << 20,
    )?;
    if offsets.len() as u64 != n || counts.len() as u64 != n {
        bail!(
            "tile table has {} entries, expected {n} (multi-sample planar tiles are not supported)",
            offsets.len()
        );
    }
    Ok(TiledLevel {
        width,
        height,
        tile_width: tw,
        tile_height: th,
        offsets,
        counts,
    })
}

impl TiledTiff {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let len = file.metadata()?.len();
        let mut hdr = [0u8; 16];
        file.read_exact_at(&mut hdr[..8], 0)?;
        let le = match &hdr[..2] {
            b"II" => true,
            b"MM" => false,
            _ => bail!("{}: not a TIFF file", path.display()),
        };
        let probe = Reader {
            f: &file,
            big: false,
            le,
        };
        let magic = probe.u(&hdr[2..4]);
        let big = match magic {
            42 => false,
            43 => true,
            m => bail!("{}: unknown TIFF version {m}", path.display()),
        };
        let r = Reader { f: &file, big, le };
        let first = if big {
            file.read_exact_at(&mut hdr[8..16], 8)?;
            r.u(&hdr[8..16])
        } else {
            r.u(&hdr[4..8])
        };
        let (entries, _) = read_ifd(&r, first)?;
        let get = |t: u16| entries.iter().find(|e| e.tag == t);
        let one = |t: u16, default: u64| -> Result<u64> {
            match get(t) {
                Some(e) => Ok(values(&r, e, 64)?.first().copied().unwrap_or(default)),
                None => Ok(default),
            }
        };
        let bits = one(258, 1)? as u16;
        let samples = one(277, 1)? as u16;
        let compression = one(259, 1)? as u16;
        let sample_format = one(339, 1)? as u16;
        let description = get(270).map(|e| ascii(&r, e)).transpose()?;
        let mut levels =
            vec![level_from(&r, &entries).with_context(|| format!("{}: level 0", path.display()))?];
        if let Some(e) = get(330) {
            for (i, off) in values(&r, e, 64)?.into_iter().enumerate() {
                let (sub, _) = read_ifd(&r, off)?;
                levels.push(
                    level_from(&r, &sub)
                        .with_context(|| format!("{}: SubIFD {i}", path.display()))?,
                );
            }
        }
        Ok(TiledTiff {
            path: path.to_path_buf(),
            file,
            len,
            levels,
            bits_per_sample: bits,
            samples_per_pixel: samples,
            compression,
            sample_format,
            description,
        })
    }

    /// Raw (still compressed) bytes of one tile.
    pub fn read_tile(&self, level: usize, tx: u32, ty: u32) -> Result<Vec<u8>> {
        let l = self
            .levels
            .get(level)
            .ok_or_else(|| anyhow!("no pyramid level {level}"))?;
        if tx >= l.tiles_across() || ty >= l.tiles_down() {
            bail!("tile ({tx}, {ty}) outside level {level}");
        }
        let i = (ty * l.tiles_across() + tx) as usize;
        let (off, n) = (l.offsets[i], l.counts[i]);
        if off.checked_add(n).is_none_or(|end| end > self.len) || n > (256 << 20) {
            bail!(
                "{}: tile {i} of level {level} points outside the file",
                self.path.display()
            );
        }
        let mut b = vec![0u8; n as usize];
        self.file.read_exact_at(&mut b, off)?;
        Ok(b)
    }

    /// The channel this file holds, from the OME-XML (the TiffData entry naming this
    /// file gives the channel index), else the file name.
    pub fn channel_name(&self) -> String {
        let file = self
            .path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.description
            .as_deref()
            .and_then(|xml| ome_channel_for_file(xml, &file))
            .unwrap_or_else(|| {
                file.trim_end_matches(".ome.tif")
                    .trim_end_matches(".tif")
                    .to_string()
            })
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')? + i;
    Some(&tag[i..j])
}

/// `Channel` name for the plane stored in `file`, from an OME-XML header that may
/// describe a multi-file dataset (one file per channel).
pub fn ome_channel_for_file(xml: &str, file: &str) -> Option<String> {
    let channels: Vec<String> = xml
        .match_indices("<Channel ")
        .filter_map(|(i, _)| {
            let end = xml[i..].find('>')? + i;
            attr(&xml[i..end], "Name").map(|s| s.to_string())
        })
        .collect();
    if channels.len() == 1 {
        return channels.into_iter().next();
    }
    for (i, _) in xml.match_indices("<TiffData") {
        let end = xml[i..]
            .find("</TiffData>")
            .map(|j| j + i)
            .or_else(|| xml[i..].find("/>").map(|j| j + i))?;
        let block = &xml[i..end];
        let names_this_file = block.contains(&format!("FileName=\"{file}\""));
        if names_this_file {
            let c: usize = attr(block, "FirstC").unwrap_or("0").parse().ok()?;
            return channels.get(c).cloned();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_from_multi_file_ome() {
        let xml = r#"<OME><Image><Pixels><Channel ID="C0" Name="DAPI"/><Channel ID="C1" Name="ATP1A1/CD45/E-Cadherin"/>
            <TiffData FirstC="0" IFD="0"><UUID FileName="ch0000_dapi.ome.tif">urn:a</UUID></TiffData>
            <TiffData FirstC="1" IFD="0"><UUID FileName="ch0001_atp1a1_cd45_e-cadherin.ome.tif">urn:b</UUID></TiffData>
            </Pixels></Image></OME>"#;
        assert_eq!(
            ome_channel_for_file(xml, "ch0001_atp1a1_cd45_e-cadherin.ome.tif").as_deref(),
            Some("ATP1A1/CD45/E-Cadherin")
        );
        assert_eq!(
            ome_channel_for_file(xml, "ch0000_dapi.ome.tif").as_deref(),
            Some("DAPI")
        );
        assert_eq!(ome_channel_for_file(xml, "other.tif"), None);
        assert_eq!(
            ome_channel_for_file(r#"<Channel ID="C0" Name="DAPI">"#, "x.tif").as_deref(),
            Some("DAPI")
        );
    }
}

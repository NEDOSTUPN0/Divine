//! The icon a Windows program carries, read straight out of its `.exe`: the
//! same picture Explorer and Steam show for it.
//!
//! An icon lives in the resource section as a group (`RT_GROUP_ICON`) listing
//! the sizes on offer, each stored separately as an `RT_ICON`. Explorer takes
//! the first group; of its sizes this takes the biggest, deepest one. A size
//! is either a PNG, which is what 256 px icons usually are, or a bare Windows
//! bitmap with no file header, which is decoded here.
//!
//! Game executables run to hundreds of megabytes, so only the headers and the
//! one icon are read, never the file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
/// No icon in an executable comes anywhere near this; a bigger number is a
/// broken or hostile file.
const MAX_ICON_BYTES: u32 = 4 * 1024 * 1024;

/// What an executable's icon turned out to be.
#[derive(Debug)]
pub enum Icon {
    /// Already a PNG, to be written out as it is.
    Png(Vec<u8>),
    /// Decoded from a bitmap: straight (not premultiplied) RGBA, top row first.
    Rgba {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

/// The biggest icon in the executable's first icon group.
pub fn read(exe: &Path) -> Result<Icon> {
    let mut file = File::open(exe).with_context(|| format!("opening {}", exe.display()))?;
    let pe = Pe::parse(&mut file).with_context(|| format!("reading {}", exe.display()))?;

    let group = pe
        .first_resource(&mut file, RT_GROUP_ICON, None)?
        .context("the program has no icon")?;
    let entries = group_entries(&group)?;
    let best = entries
        .iter()
        .max_by_key(|entry| (entry.size, entry.bit_count))
        .context("the icon group is empty")?;
    let data = pe
        .first_resource(&mut file, RT_ICON, Some(best.id))?
        .with_context(|| format!("icon {} of the group is missing", best.id))?;

    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(Icon::Png(data));
    }
    decode_dib(&data)
}

/// One size on offer in a group.
#[derive(Debug, PartialEq)]
struct GroupEntry {
    /// In pixels; the format writes 256 as 0.
    size: u32,
    bit_count: u16,
    id: u32,
}

/// `GRPICONDIR`: a six byte header, then fourteen bytes per size.
fn group_entries(group: &[u8]) -> Result<Vec<GroupEntry>> {
    let count = usize::from(u16_at(group, 4)?);
    (0..count)
        .map(|index| {
            let at = 6 + index * 14;
            let width = *group.get(at).context("the icon group is cut short")?;
            Ok(GroupEntry {
                size: if width == 0 { 256 } else { u32::from(width) },
                bit_count: u16_at(group, at + 6)?,
                id: u32::from(u16_at(group, at + 12)?),
            })
        })
        .collect()
}

/// A bitmap as an icon stores it: a `BITMAPINFOHEADER` whose height counts
/// the colour rows and the transparency mask after them, a palette for the
/// depths that have one, the colour rows bottom up, then the one bit mask.
fn decode_dib(data: &[u8]) -> Result<Icon> {
    let header = u32_at(data, 0)? as usize;
    let width = i32_at(data, 4)?;
    let height = i32_at(data, 8)? / 2;
    let bits = u16_at(data, 14)?;
    ensure!(
        (1..=256).contains(&width) && (1..=256).contains(&height),
        "an icon of {width}x{height} is not one"
    );
    let (width, height) = (width as usize, height as usize);
    let palette_len = match bits {
        1 | 4 | 8 => match u32_at(data, 32)? {
            0 => 1 << bits,
            used => used as usize,
        },
        24 | 32 => 0,
        other => bail!("icons of {other} bits per pixel are not supported"),
    };
    let palette_at = header;
    let colours_at = palette_at + palette_len * 4;
    // Rows are padded to four bytes.
    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let mask_at = colours_at + stride * height;
    let mask_stride = width.div_ceil(32) * 4;

    let mut pixels = vec![0u8; width * height * 4];
    let mut any_alpha = false;
    for y in 0..height {
        let row = colours_at + (height - 1 - y) * stride;
        for x in 0..width {
            let (r, g, b, a) = match bits {
                32 | 24 => {
                    let at = row + x * usize::from(bits / 8);
                    let px = data.get(at..at + 3).context("the icon is cut short")?;
                    let a = if bits == 32 {
                        *data.get(at + 3).context("the icon is cut short")?
                    } else {
                        255
                    };
                    (px[2], px[1], px[0], a)
                }
                _ => {
                    let per_byte = 8 / usize::from(bits);
                    let byte = *data
                        .get(row + x / per_byte)
                        .context("the icon is cut short")?;
                    let shift = 8 - usize::from(bits) * (x % per_byte + 1);
                    let index = (byte >> shift) & ((1u16 << bits) - 1) as u8;
                    let at = palette_at + usize::from(index) * 4;
                    let px = data.get(at..at + 3).context("the palette is cut short")?;
                    (px[2], px[1], px[0], 255)
                }
            };
            any_alpha |= bits == 32 && a != 0;
            pixels[(y * width + x) * 4..][..4].copy_from_slice(&[r, g, b, a]);
        }
    }

    // A 32-bit icon carries its transparency in the alpha bytes. Anything
    // shallower, and a 32-bit one written with every alpha byte zero, which
    // old tools did, carries it in the mask instead.
    if !any_alpha {
        for y in 0..height {
            let row = mask_at + (height - 1 - y) * mask_stride;
            for x in 0..width {
                // A mask that is not there leaves the icon opaque.
                let clear = data
                    .get(row + x / 8)
                    .is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0);
                pixels[(y * width + x) * 4 + 3] = if clear { 0 } else { 255 };
            }
        }
    }

    Ok(Icon::Rgba {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

/// Just enough of a PE file to find its resources.
struct Pe {
    sections: Vec<Section>,
    /// File offset of the resource directory's root, which every offset in
    /// the tree counts from.
    root: u64,
}

struct Section {
    rva: u32,
    size: u32,
    offset: u32,
}

impl Pe {
    fn parse(file: &mut File) -> Result<Self> {
        let dos = read_at(file, 0, 64)?;
        ensure!(dos.starts_with(b"MZ"), "not a Windows program");
        let pe_at = u64::from(u32_at(&dos, 0x3C)?);

        let coff = read_at(file, pe_at, 24)?;
        ensure!(coff.starts_with(b"PE\0\0"), "not a Windows program");
        let sections = usize::from(u16_at(&coff, 6)?);
        let optional_len = usize::from(u16_at(&coff, 20)?);
        let optional = read_at(file, pe_at + 24, optional_len)?;
        // Data directories sit after the fields whose width differs between
        // 32 and 64-bit programs.
        let directories = match u16_at(&optional, 0)? {
            0x10b => 96,
            0x20b => 112,
            other => bail!("an optional header of kind {other:#x}"),
        };
        const RESOURCE: usize = 2;
        let root_rva = u32_at(&optional, directories + RESOURCE * 8)?;
        ensure!(root_rva != 0, "the program has no resources");

        let table = read_at(file, pe_at + 24 + optional_len as u64, sections * 40)?;
        let sections = (0..sections)
            .map(|index| {
                let at = index * 40;
                Ok(Section {
                    size: u32_at(&table, at + 8)?.max(u32_at(&table, at + 16)?),
                    rva: u32_at(&table, at + 12)?,
                    offset: u32_at(&table, at + 20)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut pe = Pe { sections, root: 0 };
        pe.root = pe.offset(root_rva)?;
        Ok(pe)
    }

    fn offset(&self, rva: u32) -> Result<u64> {
        self.sections
            .iter()
            .find(|section| rva >= section.rva && rva - section.rva < section.size)
            .map(|section| u64::from(rva - section.rva) + u64::from(section.offset))
            .context("an address outside every section")
    }

    /// The data of the first resource of a type, or of the one with this id,
    /// in whichever language comes first.
    fn first_resource(
        &self,
        file: &mut File,
        kind: u32,
        id: Option<u32>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(types) = self.find(file, 0, Some(kind))? else {
            return Ok(None);
        };
        let Some(names) = self.find(file, subdirectory(types)?, id)? else {
            return Ok(None);
        };
        let Some(languages) = self.find(file, subdirectory(names)?, None)? else {
            return Ok(None);
        };
        ensure!(
            languages & 0x8000_0000 == 0,
            "the resource tree is deeper than it should be"
        );
        let leaf = read_at(file, self.root + u64::from(languages), 8)?;
        let size = u32_at(&leaf, 4)?;
        ensure!(size <= MAX_ICON_BYTES, "a resource of {size} bytes");
        let at = self.offset(u32_at(&leaf, 0)?)?;
        Ok(Some(read_at(file, at, size as usize)?))
    }

    /// The entry of a resource directory with this id, or its first entry,
    /// as the offset it points to. Named entries come first in a directory,
    /// and only a request for the first takes one.
    fn find(&self, file: &mut File, directory: u32, id: Option<u32>) -> Result<Option<u32>> {
        let at = self.root + u64::from(directory);
        let header = read_at(file, at, 16)?;
        let count = usize::from(u16_at(&header, 12)?) + usize::from(u16_at(&header, 14)?);
        let entries = read_at(file, at + 16, count * 8)?;
        for index in 0..count {
            let name = u32_at(&entries, index * 8)?;
            let target = u32_at(&entries, index * 8 + 4)?;
            match id {
                None => return Ok(Some(target)),
                Some(id) if name == id => return Ok(Some(target)),
                _ => {}
            }
        }
        Ok(None)
    }
}

fn subdirectory(entry: u32) -> Result<u32> {
    ensure!(
        entry & 0x8000_0000 != 0,
        "the resource tree ends sooner than it should"
    );
    Ok(entry & 0x7FFF_FFFF)
}

fn read_at(file: &mut File, at: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)
        .context("the program ends sooner than its headers say")?;
    Ok(bytes)
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    let slice = bytes.get(at..at + 2).context("a header is cut short")?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let slice = bytes.get(at..at + 4).context("a header is cut short")?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn i32_at(bytes: &[u8], at: usize) -> Result<i32> {
    Ok(u32_at(bytes, at)? as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 icon as a 32-bit bitmap: header, bottom row first in BGRA, then
    /// the mask, which a 32-bit icon with real alpha ignores.
    fn dib(bits: u16, rows: &[u8], palette: &[u8], mask: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&4i32.to_le_bytes()); // colour rows and mask rows
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&bits.to_le_bytes());
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(&((palette.len() / 4) as u32).to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        data.extend_from_slice(palette);
        data.extend_from_slice(rows);
        data.extend_from_slice(mask);
        data
    }

    #[test]
    fn a_bitmap_comes_out_top_row_first_in_rgba() {
        // Bottom row: blue, green. Top row: red, half-transparent white.
        let rows = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 128,
        ];
        let Icon::Rgba {
            width,
            height,
            pixels,
        } = decode_dib(&dib(32, &rows, &[], &[0; 8])).unwrap()
        else {
            panic!("a bitmap decoded as something else");
        };
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            pixels,
            [255, 0, 0, 255, 255, 255, 255, 128, 0, 0, 255, 255, 0, 255, 0, 255]
        );
    }

    /// Old icons keep their transparency in the one bit mask, a set bit being
    /// a clear pixel.
    #[test]
    fn a_paletted_icon_takes_its_transparency_from_the_mask() {
        let palette = [0, 0, 0, 0, 0, 0, 255, 0]; // black, red
                                                  // One byte per row holds both pixels at 1 bit each, padded to four.
        let rows = [0b0100_0000, 0, 0, 0, 0b1000_0000, 0, 0, 0];
        let mask = [0b0100_0000, 0, 0, 0, 0, 0, 0, 0];
        let Icon::Rgba { pixels, .. } = decode_dib(&dib(1, &rows, &palette, &mask)).unwrap() else {
            panic!("a bitmap decoded as something else");
        };
        // Top row: red, black. Bottom row: black, red but masked out.
        assert_eq!(
            pixels,
            [255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 0, 0]
        );
    }

    #[test]
    fn a_group_lists_256_as_the_zero_it_is_written_as() {
        let mut group = vec![0, 0, 1, 0, 2, 0];
        group.extend_from_slice(&[32, 32, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 1, 0]);
        group.extend_from_slice(&[0, 0, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 2, 0]);
        let entries = group_entries(&group).unwrap();
        assert_eq!(
            entries[1],
            GroupEntry {
                size: 256,
                bit_count: 32,
                id: 2
            }
        );
    }

    #[test]
    fn something_that_is_not_a_program_is_refused() {
        let file = std::env::temp_dir().join(format!("divine-not-an-exe-{}", std::process::id()));
        std::fs::write(&file, b"#!/bin/sh\necho hello\n").unwrap();
        assert!(read(&file).is_err());
        std::fs::remove_file(&file).unwrap();
    }
}

//! Write a Windows resource file (`.res`) from the generated icon, in pure Rust.
//!
//! `build.rs` hands the resulting file to `link.exe` as an extra input, which is how the
//! application icon and its version information reach `depth.exe`. Doing this here
//! rather than through `winresource`/`embed-resource` keeps the build dependency-free and
//! means no resource compiler (`rc.exe`) has to be installed.
//!
//! The layout follows the resource-compiler format: a null entry first, then one `RT_ICON`
//! per image in the `.ico`, an `RT_GROUP_ICON` that lists them, and an `RT_VERSION` block.
//! Header fields and memory flags mirror what the Windows SDK's `rc.exe` emits for the same
//! icon, so `link.exe` sees a file it would have produced itself.

/// One image from an `.ico` file.
pub struct Image<'a> {
    pub width: u16,
    pub height: u16,
    pub planes: u16,
    pub bit_count: u16,
    /// The image exactly as stored in the `.ico`: a `BITMAPINFOHEADER` DIB, or PNG bytes.
    pub bytes: &'a [u8],
}

/// The strings and numbers that end up in the file's Properties → Details tab.
pub struct VersionInfo<'a> {
    pub product: &'a str,
    pub description: &'a str,
    pub company: &'a str,
    pub internal_name: &'a str,
    pub original_filename: &'a str,
    pub copyright: &'a str,
    /// `(major, minor, patch, build)`; the fourth field is usually zero for a crate version.
    pub version: (u16, u16, u16, u16),
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

/// US English, Unicode. Every entry is written with the same language so the linker groups
/// them into one resource directory language.
const LANGUAGE: u16 = 0x0409;
const CODEPAGE_UNICODE: u16 = 1200;

/// Memory flags seen in `rc.exe` output; they describe how the loader may treat each block.
const FLAGS_ICON: u16 = 0x1010; // MOVEABLE | DISCARDABLE
const FLAGS_GROUP_ICON: u16 = 0x1030; // MOVEABLE | PURE | DISCARDABLE
const FLAGS_VERSION: u16 = 0x0030; // MOVEABLE | PURE

/// Read an `.ico` file into its images, in file order (smallest first, as the generator writes it).
pub fn parse_ico(bytes: &[u8]) -> Result<Vec<Image<'_>>, String> {
    if bytes.len() < 6 {
        return Err("shorter than an icon header".into());
    }
    let reserved = u16::from_le_bytes([bytes[0], bytes[1]]);
    let kind = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]);
    if reserved != 0 || kind != 1 {
        return Err(format!(
            "not an icon file (reserved {reserved}, type {kind})"
        ));
    }
    if count == 0 {
        return Err("contains no images".into());
    }
    if bytes.len() < 6 + 16 * count as usize {
        return Err("truncated directory".into());
    }

    let mut images = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let entry = &bytes[6 + index * 16..6 + index * 16 + 16];
        let size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| format!("image {index} has a bad offset"))?;
        if size == 0 || end > bytes.len() {
            return Err(format!("image {index} runs past the end of the file"));
        }
        // A stored dimension of zero means 256: the field is only one byte wide.
        images.push(Image {
            width: match entry[0] {
                0 => 256,
                width => width as u16,
            },
            height: match entry[1] {
                0 => 256,
                height => height as u16,
            },
            planes: u16::from_le_bytes([entry[4], entry[5]]),
            bit_count: u16::from_le_bytes([entry[6], entry[7]]),
            bytes: &bytes[offset..end],
        });
    }
    Ok(images)
}

/// Build the `.res` file for `images` plus a `VERSIONINFO` block.
pub fn write_res(images: &[Image<'_>], info: &VersionInfo<'_>) -> Vec<u8> {
    let mut buf = Vec::new();

    // The first entry in a resource file is always an empty entry with ordinal type and name.
    push_u32(&mut buf, 0); // DataSize
    push_u32(&mut buf, 32); // HeaderSize, including these two fields
    push_u16(&mut buf, 0xFFFF);
    push_u16(&mut buf, 0);
    push_u16(&mut buf, 0xFFFF);
    push_u16(&mut buf, 0);
    push_u32(&mut buf, 0); // DataVersion
    push_u16(&mut buf, 0); // MemoryFlags
    push_u16(&mut buf, 0); // LanguageId
    push_u32(&mut buf, 0); // Version
    push_u32(&mut buf, 0); // Characteristics

    // One RT_ICON per image, named 1..n, which is what RT_GROUP_ICON below refers to.
    for (index, image) in images.iter().enumerate() {
        resource(&mut buf, RT_ICON, index as u16 + 1, FLAGS_ICON, image.bytes);
    }

    // The group tells the shell which images make up one icon.
    let mut group = Vec::new();
    push_u16(&mut group, 0); // reserved
    push_u16(&mut group, 1); // 1 = icon
    push_u16(&mut group, images.len() as u16);
    for (index, image) in images.iter().enumerate() {
        group.push(if image.width >= 256 {
            0
        } else {
            image.width as u8
        });
        group.push(if image.height >= 256 {
            0
        } else {
            image.height as u8
        });
        group.push(0); // colour count: 0 for 32-bpp
        group.push(0); // reserved
        push_u16(&mut group, image.planes);
        push_u16(&mut group, image.bit_count);
        push_u32(&mut group, image.bytes.len() as u32);
        push_u16(&mut group, index as u16 + 1);
    }
    resource(&mut buf, RT_GROUP_ICON, 1, FLAGS_GROUP_ICON, &group);
    resource(&mut buf, RT_VERSION, 1, FLAGS_VERSION, &version_info(info));

    buf
}

/// A single resource: sizes, an ordinal type and name, then the data, padded to a DWORD.
fn resource(buf: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    push_u32(buf, data.len() as u32);
    push_u32(buf, 32); // HeaderSize: type + name + the five fields below, plus the two above
    push_u16(buf, 0xFFFF);
    push_u16(buf, kind);
    push_u16(buf, 0xFFFF);
    push_u16(buf, name);
    push_u32(buf, 0); // DataVersion
    push_u16(buf, flags);
    push_u16(buf, LANGUAGE);
    push_u32(buf, 0); // Version
    push_u32(buf, 0); // Characteristics
    buf.extend_from_slice(data);
    align(buf);
}

/// The `VS_VERSIONINFO` structure: fixed file info, then the string and translation tables.
fn version_info(info: &VersionInfo<'_>) -> Vec<u8> {
    let mut buf = Vec::new();
    let root = buf.len();
    push_u16(&mut buf, 0); // wLength, patched at the end
    push_u16(&mut buf, 52); // wValueLength: size of VS_FIXEDFILEINFO
    push_u16(&mut buf, 0); // wType: binary
    push_wide_str(&mut buf, "VS_VERSION_INFO");
    align(&mut buf);

    let (major, minor, patch, build) = info.version;
    let (file_ms, file_ls) = (pack(major, minor), pack(patch, build));
    for field in [
        0xFEEF04BD,  // dwSignature
        0x0001_0000, // dwStrucVersion
        file_ms,
        file_ls,
        file_ms, // product version mirrors the file version
        file_ls,
        0x3F,        // dwFileFlagsMask
        0x00,        // dwFileFlags
        0x0004_0004, // dwFileOS: VOS_NT_WINDOWS32
        0x01,        // dwFileType: VFT_APP
        0x00,        // dwFileSubtype
        0x00,        // dwFileDateMS
        0x00,        // dwFileDateLS
    ] {
        push_u32(&mut buf, field);
    }

    let string_file_info = begin_node(&mut buf, 0, 1, "StringFileInfo");
    // The table is named for the language and codepage it holds: 0x0409, codepage 1200.
    let table = begin_node(&mut buf, 0, 1, "040904b0");
    let version_text = version_text(info.version);
    let table_end = string_nodes(
        &mut buf,
        &[
            ("CompanyName", info.company),
            ("FileDescription", info.description),
            ("FileVersion", version_text.as_str()),
            ("InternalName", info.internal_name),
            ("LegalCopyright", info.copyright),
            ("OriginalFilename", info.original_filename),
            ("ProductName", info.product),
            ("ProductVersion", version_text.as_str()),
        ],
    );
    close_node(&mut buf, table, table_end);
    close_node(&mut buf, string_file_info, table_end);

    // VarFileInfo is the root's last child, so its end is where the whole block ends.
    let var_file_info = begin_node(&mut buf, 0, 1, "VarFileInfo");
    // The translation is a language/codepage pair, so its value is binary and four bytes long.
    let translation = begin_node(&mut buf, 4, 0, "Translation");
    push_u16(&mut buf, LANGUAGE);
    push_u16(&mut buf, CODEPAGE_UNICODE);
    let translation_end = buf.len();
    let var_end = close_node(&mut buf, translation, translation_end);
    let var_end = close_node(&mut buf, var_file_info, var_end);

    close_node(&mut buf, root, var_end);
    buf
}

/// Start a container node (StringFileInfo, a string table, VarFileInfo, Translation).
fn begin_node(buf: &mut Vec<u8>, value_length: u16, kind: u16, key: &str) -> usize {
    let start = buf.len();
    push_u16(buf, 0); // wLength, patched by close_node
    push_u16(buf, value_length);
    push_u16(buf, kind);
    push_wide_str(buf, key);
    align(buf);
    start
}

/// Close a node opened with [`begin_node`] and return where its content ends.
///
/// `wLength` covers the node's own bytes and stops at the last one; the padding that follows,
/// aligning the next node, is not counted. `rc.exe` writes lengths this way, so matching it
/// keeps the file within the bounds of anything that walks the tree.
fn close_node(buf: &mut Vec<u8>, start: usize, content_end: usize) -> usize {
    let length = (content_end - start) as u16;
    buf[start..start + 2].copy_from_slice(&length.to_le_bytes());
    align(buf);
    content_end
}

/// `"Key" = "Value"`, the leaf of a string table. Returns where the node's content ends.
fn string_node(buf: &mut Vec<u8>, key: &str, value: &str) -> usize {
    let start = buf.len();
    push_u16(buf, 0); // wLength
    // For text nodes this counts UTF-16 code units including the terminator.
    push_u16(buf, value.encode_utf16().count() as u16 + 1);
    push_u16(buf, 1); // wType: text
    push_wide_str(buf, key);
    align(buf);
    push_wide_str(buf, value);
    let end = buf.len();
    close_node(buf, start, end)
}

/// Write every leaf of a string table and return where the last one's content ends.
fn string_nodes(buf: &mut Vec<u8>, entries: &[(&str, &str)]) -> usize {
    let mut end = buf.len();
    for (key, value) in entries {
        end = string_node(buf, key, value);
    }
    end
}

/// Pack two halves into the `dwFileVersionMS`/`LS` style DWORD.
fn pack(high: u16, low: u16) -> u32 {
    ((high as u32) << 16) | low as u32
}

/// `0.1.0.0`, the dotted form the version block stores as text.
fn version_text(version: (u16, u16, u16, u16)) -> String {
    let (major, minor, patch, build) = version;
    format!("{major}.{minor}.{patch}.{build}")
}

fn push_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// UTF-16LE with a terminating null, as every string in the version block is stored.
fn push_wide_str(buf: &mut Vec<u8>, value: &str) {
    for unit in value.encode_utf16() {
        push_u16(buf, unit);
    }
    push_u16(buf, 0);
}

/// Resource data and node values start on DWORD boundaries.
fn align(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

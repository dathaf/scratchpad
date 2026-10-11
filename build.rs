//! Embeds app.ico and app.manifest into the .exe as Windows resources.
//!
//! The GNU toolchain that rustup installs has a linker but no resource compiler (windres),
//! so this writes the tiny COFF object a resource compiler would produce by hand:
//! one `.rsrc` section holding the resource directory tree and the data.

use std::path::PathBuf;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_MANIFEST: u32 = 24;
const LANGUAGE_NEUTRAL: u32 = 0;

struct Resource {
    kind: u32,
    id: u32,
    data: Vec<u8>,
}

fn u16le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Splits an .ico file into one RT_ICON per image plus the RT_GROUP_ICON that lists them.
fn icon_resources(ico: &[u8]) -> Vec<Resource> {
    let count = u16le(ico, 4) as usize;
    let mut resources = Vec::new();
    let mut group = vec![0, 0, 1, 0];
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let entry = &ico[6 + i * 16..6 + i * 16 + 16];
        let (size, offset) = (u32le(entry, 8) as usize, u32le(entry, 12) as usize);
        let id = i as u32 + 1;
        resources.push(Resource { kind: RT_ICON, id, data: ico[offset..offset + size].to_vec() });
        // Group entry = the file's directory entry with the file offset replaced by the resource id.
        group.extend(&entry[..12]);
        group.extend((id as u16).to_le_bytes());
    }
    resources.push(Resource { kind: RT_GROUP_ICON, id: 1, data: group });
    resources
}

fn directory(out: &mut Vec<u8>, entries: &[(u32, u32)]) {
    out.extend([0u8; 12]); // characteristics, timestamp, version
    out.extend(0u16.to_le_bytes()); // named entries
    out.extend((entries.len() as u16).to_le_bytes()); // id entries
    for (id, offset) in entries {
        out.extend(id.to_le_bytes());
        out.extend(offset.to_le_bytes());
    }
}

/// Builds the .rsrc section: type directory -> id directories -> language directories ->
/// data entries -> data. Returns the section bytes and the offsets of every field that
/// must be relocated to an image-relative address.
fn resource_section(mut resources: Vec<Resource>) -> (Vec<u8>, Vec<u32>) {
    const SUBDIRECTORY: u32 = 0x8000_0000;
    resources.sort_by_key(|r| (r.kind, r.id));
    let mut kinds: Vec<u32> = resources.iter().map(|r| r.kind).collect();
    kinds.dedup();

    let dir_size = |entries: usize| 16 + 8 * entries as u32;
    // Offsets of each block, in the order they are written.
    let mut offset = dir_size(kinds.len());
    let kind_dirs: Vec<u32> = kinds.iter().map(|kind| {
        let at = offset;
        offset += dir_size(resources.iter().filter(|r| r.kind == *kind).count());
        at
    }).collect();
    let language_dirs: Vec<u32> = resources.iter().map(|_| { let at = offset; offset += dir_size(1); at }).collect();
    let data_entries: Vec<u32> = resources.iter().map(|_| { let at = offset; offset += 16; at }).collect();
    let data: Vec<u32> = resources.iter().map(|r| { let at = offset; offset += (r.data.len() as u32 + 3) & !3; at }).collect();

    let mut out = Vec::with_capacity(offset as usize);
    directory(&mut out, &kinds.iter().zip(&kind_dirs).map(|(kind, at)| (*kind, at | SUBDIRECTORY)).collect::<Vec<_>>());
    for kind in &kinds {
        let entries: Vec<(u32, u32)> = resources.iter().enumerate().filter(|(_, r)| r.kind == *kind)
            .map(|(i, r)| (r.id, language_dirs[i] | SUBDIRECTORY)).collect();
        directory(&mut out, &entries);
    }
    for at in &data_entries {
        directory(&mut out, &[(LANGUAGE_NEUTRAL, *at)]);
    }
    let mut relocations = Vec::new();
    for (i, resource) in resources.iter().enumerate() {
        relocations.push(out.len() as u32);
        out.extend(data[i].to_le_bytes()); // section-relative for now; the linker adds the section's address
        out.extend((resource.data.len() as u32).to_le_bytes());
        out.extend([0u8; 8]); // code page, reserved
    }
    for resource in &resources {
        out.extend(&resource.data);
        out.resize((out.len() + 3) & !3, 0);
    }
    (out, relocations)
}

/// Wraps the section in a minimal x64 COFF object file.
fn coff_object(section: &[u8], relocations: &[u32]) -> Vec<u8> {
    const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
    const IMAGE_REL_AMD64_ADDR32NB: u16 = 3;
    // initialized data | 4-byte alignment | readable | writable (what windres emits)
    const SECTION_FLAGS: u32 = 0xC030_0040;
    let section_at = 20 + 40u32;
    let relocations_at = section_at + section.len() as u32;
    let symbols_at = relocations_at + 10 * relocations.len() as u32;

    let mut out = Vec::new();
    // File header
    out.extend(IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // sections
    out.extend(0u32.to_le_bytes()); // timestamp
    out.extend(symbols_at.to_le_bytes());
    out.extend(2u32.to_le_bytes()); // symbol table entries (section symbol + its aux record)
    out.extend(0u16.to_le_bytes()); // optional header size
    out.extend(0u16.to_le_bytes()); // characteristics
    // Section header
    out.extend(b".rsrc\0\0\0");
    out.extend(0u32.to_le_bytes()); // virtual size
    out.extend(0u32.to_le_bytes()); // virtual address
    out.extend((section.len() as u32).to_le_bytes());
    out.extend(section_at.to_le_bytes());
    out.extend(relocations_at.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // line numbers
    out.extend((relocations.len() as u16).to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(SECTION_FLAGS.to_le_bytes());
    out.extend(section);
    for at in relocations {
        out.extend(at.to_le_bytes());
        out.extend(0u32.to_le_bytes()); // symbol 0 = the .rsrc section
        out.extend(IMAGE_REL_AMD64_ADDR32NB.to_le_bytes());
    }
    // Symbol table: the section symbol and its auxiliary record
    out.extend(b".rsrc\0\0\0");
    out.extend(0u32.to_le_bytes()); // value
    out.extend(1i16.to_le_bytes()); // section number
    out.extend(0u16.to_le_bytes()); // type
    out.push(3); // storage class: static
    out.push(1); // aux records
    out.extend((section.len() as u32).to_le_bytes());
    out.extend((relocations.len() as u16).to_le_bytes());
    out.extend([0u8; 12]);
    out.extend(4u32.to_le_bytes()); // empty string table
    out
}

fn main() {
    println!("cargo:rerun-if-changed=app.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    let mut resources = icon_resources(&std::fs::read("app.ico").expect("app.ico"));
    resources.push(Resource { kind: RT_MANIFEST, id: 1, data: std::fs::read("app.manifest").expect("app.manifest") });
    let (section, relocations) = resource_section(resources);

    let object = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("resources.o");
    std::fs::write(&object, coff_object(&section, &relocations)).expect("write resources.o");
    println!("cargo:rustc-link-arg-bins={}", object.display());
}

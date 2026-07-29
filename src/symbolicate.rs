use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rustc_demangle::demangle;
use wholesym::{LookupAddress, MultiArchDisambiguator, SymbolManager, SymbolManagerConfig};

use crate::profile::{Library, Profile};
use crate::tree::AddressFrame;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ResolvedSymbol {
    pub name: String,
    pub lib_name: String,
}

pub type SymbolTable = HashMap<AddressFrame, ResolvedSymbol>;

fn prettify_symbol(name: &str) -> String {
    let demangled = demangle(name).to_string();
    demangled
        .strip_prefix('_')
        .unwrap_or(&demangled)
        .to_string()
}

pub fn fallback_label(_libs: &[Library], frame: AddressFrame) -> String {
    format!("0x{:x}", frame.address)
}

fn fallback_symbol(libs: &[Library], frame: AddressFrame) -> ResolvedSymbol {
    let lib_name = frame
        .lib_index
        .and_then(|i| libs.get(i as usize))
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "???".into());
    ResolvedSymbol {
        name: fallback_label(libs, frame),
        lib_name,
    }
}

fn disambiguator_for(lib: &Library) -> Option<MultiArchDisambiguator> {
    lib.arch
        .as_ref()
        .map(|arch| MultiArchDisambiguator::Arch(arch.clone()))
}

/// Resolve all address frames using on-disk binaries from `profile.libs`.
pub async fn symbolicate(profile: &Profile, frames: &[AddressFrame]) -> Result<SymbolTable> {
    let mut table = SymbolTable::new();
    if frames.is_empty() {
        return Ok(table);
    }

    let mut by_lib: HashMap<Option<u32>, Vec<u64>> = HashMap::new();
    for frame in frames {
        by_lib
            .entry(frame.lib_index)
            .or_default()
            .push(frame.address);
    }
    for addrs in by_lib.values_mut() {
        addrs.sort_unstable();
        addrs.dedup();
    }

    let symbol_manager = SymbolManager::with_config(SymbolManagerConfig::default());

    for (lib_index, addresses) in by_lib {
        let Some(lib_index) = lib_index else {
            for &address in &addresses {
                let frame = AddressFrame {
                    lib_index: None,
                    address,
                };
                table.insert(frame, fallback_symbol(&profile.libs, frame));
            }
            continue;
        };

        let Some(lib) = profile.libs.get(lib_index as usize) else {
            for &address in &addresses {
                let frame = AddressFrame {
                    lib_index: Some(lib_index),
                    address,
                };
                table.insert(frame, fallback_symbol(&profile.libs, frame));
            }
            continue;
        };

        let Some(path) = lib.path.as_deref().map(Path::new).filter(|p| p.exists()) else {
            for &address in &addresses {
                let frame = AddressFrame {
                    lib_index: Some(lib_index),
                    address,
                };
                table.insert(frame, fallback_symbol(&profile.libs, frame));
            }
            continue;
        };

        let symbol_map = match symbol_manager
            .load_symbol_map_for_binary_at_path(path, disambiguator_for(lib))
            .await
        {
            Ok(map) => map,
            Err(_) => {
                for &address in &addresses {
                    let frame = AddressFrame {
                        lib_index: Some(lib_index),
                        address,
                    };
                    table.insert(frame, fallback_symbol(&profile.libs, frame));
                }
                continue;
            }
        };

        for &address in &addresses {
            let frame = AddressFrame {
                lib_index: Some(lib_index),
                address,
            };
            let resolved = match symbol_map
                .lookup(LookupAddress::Relative(address as u32))
                .await
            {
                Some(info) => ResolvedSymbol {
                    name: prettify_symbol(&info.symbol.name),
                    lib_name: lib.name.clone(),
                },
                None => fallback_symbol(&profile.libs, frame),
            };
            table.insert(frame, resolved);
        }
    }

    Ok(table)
}

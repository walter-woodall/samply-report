use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use serde::Deserialize;


/// Top-level processed Firefox Profiler / samply profile.
#[derive(Debug, Deserialize)]
pub struct Profile {
    pub meta: Meta,
    pub libs: Vec<Library>,
    #[serde(default)]
    pub threads: Vec<Thread>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub product: Option<String>,
    pub oscpu: Option<String>,
    pub interval: Option<f64>,
    pub preprocessed_profile_version: Option<u32>,
    pub version: Option<u32>,
    pub symbolicated: Option<bool>,
    pub sample_units: Option<SampleUnits>,
    pub start_time: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct SampleUnits {
    pub time: Option<String>,
    #[serde(rename = "threadCPUDelta")]
    pub thread_cpu_delta: Option<String>,
    #[serde(rename = "eventDelay")]
    pub event_delay: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Library {
    pub name: String,
    pub path: Option<String>,
    pub debug_name: Option<String>,
    pub debug_path: Option<String>,
    pub breakpad_id: Option<String>,
    pub code_id: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Thread {
    pub name: Option<String>,
    #[serde(default)]
    pub is_main_thread: bool,
    #[serde(default)]
    pub pid: Option<serde_json::Value>,
    #[serde(default)]
    pub tid: Option<serde_json::Value>,
    #[serde(default)]
    pub string_array: Vec<String>,
    pub stack_table: StackTable,
    pub frame_table: FrameTable,
    pub func_table: FuncTable,
    pub resource_table: ResourceTable,
    pub samples: Samples,
}

#[derive(Debug, Deserialize)]
pub struct StackTable {
    pub length: usize,
    pub prefix: Vec<Option<i32>>,
    pub frame: Vec<i32>,
}

#[derive(Debug, Deserialize)]
pub struct FrameTable {
    pub length: usize,
    pub address: Vec<i64>,
    pub func: Vec<i32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct FuncTable {
    pub length: usize,
    pub name: Vec<i32>,
    pub resource: Vec<i32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ResourceTable {
    pub length: usize,
    /// Index into profile.libs, or null for non-library resources.
    pub lib: Vec<Option<i32>>,
    pub name: Vec<i32>,
}

#[derive(Debug, Deserialize)]
pub struct Samples {
    pub length: usize,
    pub stack: Vec<Option<i32>>,
    #[serde(default)]
    pub weight: Option<Vec<f64>>,
}

/// Load a profile from a path. Accepts plain JSON or gzip-compressed JSON.
pub fn load_profile(path: &Path) -> Result<Profile> {
    let file = File::open(path)
        .with_context(|| format!("failed to open profile {}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 2];
    {
        let buf = reader
            .fill_buf()
            .context("failed to read profile header")?;
        anyhow::ensure!(!buf.is_empty(), "profile file is empty");
        magic[0] = buf[0];
        if buf.len() > 1 {
            magic[1] = buf[1];
        }
    }

    if magic == [0x1f, 0x8b] {
        let decoder = GzDecoder::new(reader);
        serde_json::from_reader(decoder).context("failed to parse gzip-compressed profile JSON")
    } else {
        serde_json::from_reader(reader).context("failed to parse profile JSON")
    }
}

pub fn print_metadata(profile: &Profile) {
    let meta = &profile.meta;
    println!(
        "Profile: {}",
        meta.product.as_deref().unwrap_or("(unknown product)")
    );
    if let Some(os) = meta.oscpu.as_deref() {
        println!("  OS: {os}");
    }
    if let Some(interval) = meta.interval {
        let unit = meta
            .sample_units
            .as_ref()
            .and_then(|u| u.time.as_deref())
            .unwrap_or("ms");
        println!("  interval: {interval} {unit}");
    }
    if let Some(v) = meta.preprocessed_profile_version {
        println!("  preprocessedProfileVersion: {v}");
    }
    if let Some(v) = meta.version {
        println!("  version: {v}");
    }
    if let Some(s) = meta.symbolicated {
        println!("  symbolicated: {s}");
    }
    if let Some(units) = &meta.sample_units {
        let mut parts = Vec::new();
        if let Some(t) = &units.time {
            parts.push(format!("time={t}"));
        }
        if let Some(t) = &units.thread_cpu_delta {
            parts.push(format!("threadCPUDelta={t}"));
        }
        if let Some(t) = &units.event_delay {
            parts.push(format!("eventDelay={t}"));
        }
        if !parts.is_empty() {
            println!("  sampleUnits: {}", parts.join(", "));
        }
    }
    if let Some(t) = meta.start_time {
        println!("  startTime: {t}");
    }

    println!("  threads: {}", profile.threads.len());
    for (i, thread) in profile.threads.iter().enumerate() {
        let name = thread.name.as_deref().unwrap_or("(unnamed)");
        let samples = thread.samples.length;
        let main = if thread.is_main_thread { " [main]" } else { "" };
        println!("    [{i}]{main} {name}  samples={samples}");
    }

    println!("  libs: {}", profile.libs.len());
    for (i, lib) in profile.libs.iter().enumerate() {
        let arch = lib.arch.as_deref().unwrap_or("?");
        let path = lib.path.as_deref().unwrap_or("(no path)");
        println!("    [{i}] {} ({arch})  {path}", lib.name);
        if let Some(id) = lib.breakpad_id.as_deref() {
            println!("         breakpadId: {id}");
        }
    }
}

/// Pick a thread by index, or the main/heaviest thread if `None`.
pub fn select_thread(profile: &Profile, thread: Option<usize>) -> Result<(usize, &Thread)> {
    if profile.threads.is_empty() {
        bail!("profile has no threads");
    }
    if let Some(idx) = thread {
        let t = profile
            .threads
            .get(idx)
            .with_context(|| format!("thread index {idx} out of range"))?;
        return Ok((idx, t));
    }
    if let Some((idx, _)) = profile
        .threads
        .iter()
        .enumerate()
        .find(|(_, t)| t.is_main_thread)
    {
        return Ok((idx, &profile.threads[idx]));
    }
    let (idx, _) = profile
        .threads
        .iter()
        .enumerate()
        .max_by_key(|(_, t)| t.samples.length)
        .unwrap();
    Ok((idx, &profile.threads[idx]))
}

impl Thread {
    /// Resolve a stack index to a root→leaf address path.
    pub fn address_stack(&self, stack_idx: i32) -> Result<Vec<crate::tree::AddressFrame>> {
        let mut leaf_first = Vec::new();
        let mut sid = Some(stack_idx);
        while let Some(idx) = sid {
            let idx = idx as usize;
            anyhow::ensure!(
                idx < self.stack_table.length,
                "stack index {idx} out of range"
            );
            let frame_idx = self.stack_table.frame[idx] as usize;
            leaf_first.push(self.address_frame(frame_idx)?);
            sid = self.stack_table.prefix[idx];
        }
        leaf_first.reverse();
        Ok(leaf_first)
    }

    pub fn address_frame(&self, frame_idx: usize) -> Result<crate::tree::AddressFrame> {
        anyhow::ensure!(
            frame_idx < self.frame_table.length,
            "frame index {frame_idx} out of range"
        );
        let func_idx = self.frame_table.func[frame_idx] as usize;
        anyhow::ensure!(
            func_idx < self.func_table.length,
            "func index {func_idx} out of range"
        );
        let resource_idx = self.func_table.resource[func_idx];
        let lib_index = if resource_idx >= 0 {
            let resource_idx = resource_idx as usize;
            anyhow::ensure!(
                resource_idx < self.resource_table.length,
                "resource index {resource_idx} out of range"
            );
            self.resource_table.lib[resource_idx].map(|l| l as u32)
        } else {
            None
        };
        let address = self.frame_table.address[frame_idx];
        let address = if address < 0 { 0 } else { address as u64 };
        Ok(crate::tree::AddressFrame { lib_index, address })
    }
}

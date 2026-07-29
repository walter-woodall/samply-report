use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::profile::{Library, Thread};
use crate::symbolicate::{SymbolTable, fallback_label};

/// Identity of a stack frame after symbolication: library + function name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub lib_index: Option<u32>,
    pub name: Arc<str>,
}

/// Address-level frame used only while collecting addresses to symbolicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AddressFrame {
    pub lib_index: Option<u32>,
    pub address: u64,
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub frame: FrameKey,
    pub total: u64,
    pub self_count: u64,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone)]
pub struct CallTree {
    pub roots: Vec<TreeNode>,
    pub total_samples: u64,
}

#[derive(Debug, Clone)]
struct BuildNode {
    frame: FrameKey,
    total: u64,
    self_count: u64,
    children: HashMap<FrameKey, BuildNode>,
}

impl BuildNode {
    fn new(frame: FrameKey) -> Self {
        Self {
            frame,
            total: 0,
            self_count: 0,
            children: HashMap::new(),
        }
    }

    fn into_tree_node(self) -> TreeNode {
        let mut children: Vec<TreeNode> = self
            .children
            .into_values()
            .map(BuildNode::into_tree_node)
            .collect();
        children.sort_by(|a, b| {
            b.total
                .cmp(&a.total)
                .then(a.frame.name.cmp(&b.frame.name))
        });
        TreeNode {
            frame: self.frame,
            total: self.total,
            self_count: self.self_count,
            children,
        }
    }
}

fn add_path(roots: &mut HashMap<FrameKey, BuildNode>, path: &[FrameKey], weight: u64) {
    fn descend(node: &mut BuildNode, path: &[FrameKey], weight: u64) {
        node.total += weight;
        match path.split_first() {
            None => node.self_count += weight,
            Some((frame, rest)) => {
                let child = node
                    .children
                    .entry(frame.clone())
                    .or_insert_with(|| BuildNode::new(frame.clone()));
                descend(child, rest, weight);
            }
        }
    }

    if let Some((frame, rest)) = path.split_first() {
        let root = roots
            .entry(frame.clone())
            .or_insert_with(|| BuildNode::new(frame.clone()));
        descend(root, rest, weight);
    }
}

impl CallTree {
    /// Collect every address frame referenced by the thread's samples.
    pub fn collect_addresses(thread: &Thread) -> Result<Vec<AddressFrame>> {
        let mut out = Vec::new();
        for i in 0..thread.samples.length {
            let Some(stack_idx) = thread.samples.stack[i] else {
                continue;
            };
            let path = thread.address_stack(stack_idx)?;
            out.extend(path);
        }
        out.sort_by_key(|f| (f.lib_index, f.address));
        out.dedup();
        Ok(out)
    }

    /// Build a top-down call tree keyed by resolved symbol name.
    pub fn from_thread(
        thread: &Thread,
        symbols: &SymbolTable,
        libs: &[Library],
    ) -> Result<Self> {
        let mut roots: HashMap<FrameKey, BuildNode> = HashMap::new();
        let mut total_samples = 0u64;
        let mut name_cache: HashMap<AddressFrame, FrameKey> = HashMap::new();

        for i in 0..thread.samples.length {
            let Some(stack_idx) = thread.samples.stack[i] else {
                continue;
            };
            let weight = thread
                .samples
                .weight
                .as_ref()
                .and_then(|w| w.get(i).copied())
                .unwrap_or(1.0);
            let weight = weight.round().max(0.0) as u64;
            if weight == 0 {
                continue;
            }
            total_samples += weight;

            let addresses = thread
                .address_stack(stack_idx)
                .with_context(|| format!("failed to resolve sample {i} stack {stack_idx}"))?;

            let mut path = Vec::with_capacity(addresses.len());
            for addr in addresses {
                let key = name_cache
                    .entry(addr)
                    .or_insert_with(|| frame_key_for(addr, symbols, libs))
                    .clone();
                // Collapse consecutive identical frames (recursion / same PC mapped name).
                if path.last() == Some(&key) {
                    continue;
                }
                path.push(key);
            }
            add_path(&mut roots, &path, weight);
        }

        let mut roots: Vec<TreeNode> = roots.into_values().map(BuildNode::into_tree_node).collect();
        roots.sort_by(|a, b| {
            b.total
                .cmp(&a.total)
                .then(a.frame.name.cmp(&b.frame.name))
        });

        Ok(Self {
            roots,
            total_samples,
        })
    }

    pub fn flatten(
        &self,
        expanded: &std::collections::HashSet<NodePath>,
        min_pct: f64,
    ) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        walk_forest(
            &self.roots,
            &[],
            0,
            expanded,
            self.total_samples,
            min_pct,
            &mut rows,
        );
        rows
    }

    pub fn get(&self, path: &[usize]) -> Option<&TreeNode> {
        let (first, rest) = path.split_first()?;
        let mut node = self.roots.get(*first)?;
        for idx in rest {
            node = node.children.get(*idx)?;
        }
        Some(node)
    }

    /// Flatten a single subtree re-rooted at visual depth 0.
    /// The root itself is always shown (`force`), even if below `min_pct`.
    pub fn flatten_rooted(
        &self,
        root: &NodePath,
        expanded: &std::collections::HashSet<NodePath>,
        min_pct: f64,
    ) -> Vec<VisibleRow> {
        let Some(node) = self.get(root) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        push_subtree(
            node,
            root.clone(),
            0,
            expanded,
            self.total_samples,
            min_pct,
            true,
            &mut rows,
        );
        rows
    }

    /// Flatten with matching nodes re-rooted at visual depth 0.
    ///
    /// Only topmost matches become roots (a match under another match is shown
    /// by expanding the ancestor, not as its own root). Children keep relative
    /// indentation from that root.
    ///
    /// When `within` is set, search only that subtree (absolute paths preserved).
    pub fn flatten_filtered<F>(
        &self,
        expanded: &std::collections::HashSet<NodePath>,
        within: Option<&NodePath>,
        min_pct: f64,
        mut is_match: F,
    ) -> Vec<VisibleRow>
    where
        F: FnMut(&FrameKey) -> bool,
    {
        let total = self.total_samples;
        let mut match_roots: Vec<(NodePath, &TreeNode)> = Vec::new();
        match within {
            Some(root) => {
                let Some(node) = self.get(root) else {
                    return Vec::new();
                };
                if is_match(&node.frame) {
                    match_roots.push((root.clone(), node));
                } else {
                    find_topmost_matches(
                        &node.children,
                        root,
                        &mut is_match,
                        false,
                        total,
                        min_pct,
                        &mut match_roots,
                    );
                }
            }
            None => {
                find_topmost_matches(
                    &self.roots,
                    &[],
                    &mut is_match,
                    false,
                    total,
                    min_pct,
                    &mut match_roots,
                );
            }
        }

        let mut rows = Vec::new();
        for (path, node) in match_roots {
            // Matched / focused roots are always shown.
            push_subtree(
                node,
                path,
                0,
                expanded,
                total,
                min_pct,
                true,
                &mut rows,
            );
        }
        rows
    }
}

fn walk_forest(
    children: &[TreeNode],
    prefix: &[usize],
    depth: usize,
    expanded: &std::collections::HashSet<NodePath>,
    total_samples: u64,
    min_pct: f64,
    rows: &mut Vec<VisibleRow>,
) {
    for (i, node) in children.iter().enumerate() {
        if pct(node.total, total_samples) < min_pct {
            continue;
        }
        let mut path = prefix.to_vec();
        path.push(i);
        push_subtree(
            node,
            path,
            depth,
            expanded,
            total_samples,
            min_pct,
            false,
            rows,
        );
    }
}

fn visible_child_count(node: &TreeNode, total_samples: u64, min_pct: f64) -> usize {
    node.children
        .iter()
        .filter(|c| pct(c.total, total_samples) >= min_pct)
        .count()
}

fn push_subtree(
    node: &TreeNode,
    path: NodePath,
    depth: usize,
    expanded: &std::collections::HashSet<NodePath>,
    total_samples: u64,
    min_pct: f64,
    force: bool,
    rows: &mut Vec<VisibleRow>,
) {
    if !force && pct(node.total, total_samples) < min_pct {
        return;
    }
    let is_expanded = expanded.contains(&path);
    let has_children = visible_child_count(node, total_samples, min_pct) > 0;
    rows.push(VisibleRow {
        path: path.clone(),
        depth,
        frame: node.frame.clone(),
        total: node.total,
        self_count: node.self_count,
        has_children,
        expanded: is_expanded,
    });
    if is_expanded && has_children {
        for (i, child) in node.children.iter().enumerate() {
            if pct(child.total, total_samples) < min_pct {
                continue;
            }
            let mut child_path = path.clone();
            child_path.push(i);
            push_subtree(
                child,
                child_path,
                depth + 1,
                expanded,
                total_samples,
                min_pct,
                false,
                rows,
            );
        }
    }
}

fn find_topmost_matches<'a, F>(
    children: &'a [TreeNode],
    prefix: &[usize],
    is_match: &mut F,
    under_match: bool,
    total_samples: u64,
    min_pct: f64,
    out: &mut Vec<(NodePath, &'a TreeNode)>,
) where
    F: FnMut(&FrameKey) -> bool,
{
    for (i, node) in children.iter().enumerate() {
        if pct(node.total, total_samples) < min_pct {
            continue;
        }
        let mut path = prefix.to_vec();
        path.push(i);
        let matched = is_match(&node.frame);
        if matched && !under_match {
            out.push((path.clone(), node));
        }
        find_topmost_matches(
            &node.children,
            &path,
            is_match,
            under_match || matched,
            total_samples,
            min_pct,
            out,
        );
    }
}

fn frame_key_for(addr: AddressFrame, symbols: &SymbolTable, libs: &[Library]) -> FrameKey {
    let name = if let Some(sym) = symbols.get(&addr) {
        Arc::<str>::from(sym.name.as_str())
    } else {
        Arc::<str>::from(fallback_label(libs, addr).as_str())
    };
    FrameKey {
        lib_index: addr.lib_index,
        name,
    }
}

/// Path of child indices from the forest roots, used as a stable node id.
pub type NodePath = Vec<usize>;

#[derive(Debug, Clone)]
pub struct VisibleRow {
    pub path: NodePath,
    pub depth: usize,
    pub frame: FrameKey,
    pub total: u64,
    pub self_count: u64,
    pub has_children: bool,
    pub expanded: bool,
}

pub fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64) * 100.0 / (whole as f64)
    }
}

pub fn format_frame(frame: &FrameKey, libs: &[Library]) -> String {
    let lib = frame
        .lib_index
        .and_then(|i| libs.get(i as usize))
        .map(|l| l.name.as_str())
        .unwrap_or("???");
    format!("{}  [{lib}]", frame.name)
}

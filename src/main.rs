mod profile;
mod symbolicate;
mod tree;
mod ui;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

use profile::{load_profile, print_metadata, select_thread};
use symbolicate::symbolicate;
use tree::{CallTree, NodePath, format_frame, pct};
use ui::{App, run_tui};

#[derive(Debug, Parser)]
#[command(
    name = "samply-report",
    about = "perf report-style viewer for samply / Firefox Profiler profiles"
)]
struct Args {
    /// Path to a samply/Firefox profile JSON (plain or gzip-compressed)
    profile: PathBuf,

    /// Thread index (default: main thread, else heaviest)
    #[arg(long, short = 't')]
    thread: Option<usize>,

    /// Print profile metadata and exit
    #[arg(long)]
    meta: bool,

    /// Print a text call tree instead of opening the TUI
    #[arg(long)]
    tree: bool,

    /// Skip symbolication (show lib+offset only)
    #[arg(long)]
    no_symbols: bool,

    /// Expand threshold percent for --tree and TUI `e`
    #[arg(long, default_value_t = 1.0)]
    expand_pct: f64,

    /// Max depth to print with --tree (0 = unlimited)
    #[arg(long, default_value_t = 0)]
    depth: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let profile = load_profile(&args.profile)?;

    if args.meta {
        print_metadata(&profile);
        return Ok(());
    }

    let (thread_index, thread) = select_thread(&profile, args.thread)?;
    let thread_name = thread
        .name
        .clone()
        .unwrap_or_else(|| format!("thread {thread_index}"));

    eprintln!(
        "Collecting frames for thread[{thread_index}]: {thread_name} ({} samples)...",
        thread.samples.length
    );
    let addresses = CallTree::collect_addresses(thread)?;

    let symbols = if args.no_symbols {
        Default::default()
    } else {
        eprintln!("Symbolicating {} unique frames...", addresses.len());
        symbolicate(&profile, &addresses).await?
    };

    eprintln!("Building call tree...");
    let tree = CallTree::from_thread(thread, &symbols, &profile.libs)?;

    if args.tree {
        print_tree(&tree, &profile.libs, args.expand_pct, args.depth);
        return Ok(());
    }

    let app = App::new(
        &profile,
        args.profile.clone(),
        thread_index,
        thread_name,
        tree,
        args.expand_pct,
    );
    run_tui(app)
}

fn print_tree(tree: &CallTree, libs: &[profile::Library], expand_pct: f64, max_depth: usize) {
    println!(
        "{:>8}  {:>8}  {:>8}  Symbol",
        "Children", "Self", "Samples"
    );

    let mut expanded = HashSet::new();
    fn mark(
        children: &[tree::TreeNode],
        prefix: &[usize],
        total: u64,
        expand_pct: f64,
        max_depth: usize,
        depth: usize,
        expanded: &mut HashSet<NodePath>,
    ) {
        if max_depth > 0 && depth >= max_depth {
            return;
        }
        for (i, node) in children.iter().enumerate() {
            if pct(node.total, total) < expand_pct {
                continue;
            }
            if node.children.is_empty() {
                continue;
            }
            let mut path = prefix.to_vec();
            path.push(i);
            expanded.insert(path.clone());
            mark(
                &node.children,
                &path,
                total,
                expand_pct,
                max_depth,
                depth + 1,
                expanded,
            );
        }
    }
    mark(
        &tree.roots,
        &[],
        tree.total_samples,
        expand_pct,
        max_depth,
        0,
        &mut expanded,
    );
    for i in 0..tree.roots.len() {
        expanded.insert(vec![i]);
    }

    let rows = tree.flatten(&expanded);
    for row in rows {
        if max_depth > 0 && row.depth >= max_depth {
            continue;
        }
        let marker = if !row.has_children {
            " "
        } else if row.expanded {
            "-"
        } else {
            "+"
        };
        let indent = "  ".repeat(row.depth);
        let name = format_frame(&row.frame, libs);
        println!(
            "{:>7.2}%  {:>7.2}%  {:>8}  {indent}{marker} {name}",
            pct(row.total, tree.total_samples),
            pct(row.self_count, tree.total_samples),
            row.total,
        );
    }
}

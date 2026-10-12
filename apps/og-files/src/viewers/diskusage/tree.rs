//! In-memory arena of the scanned tree: `Vec<Node>` with parent/children
//! indices. Sizes and counts are kept aggregated (every node carries the
//! totals of its subtree), updated incrementally as batches arrive and as
//! nodes are removed/changed — so no pass over the whole tree is ever
//! needed to show totals.

use super::scan::{Batch, NodeKind, Probe, RawEntry, NONE};
use crate::filetype::Kind;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const UNREADABLE: u8 = 1;
const REMOVED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// allocated size (st_blocks * 512) — what actually frees space
    Alloc,
    /// st_size
    Apparent,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Box<str>,
    pub parent: u32,
    pub children: Vec<u32>,
    pub kind: NodeKind,
    /// apparent bytes of the subtree (files only)
    pub size: u64,
    /// allocated bytes of the subtree (files only)
    pub alloc: u64,
    /// files in the subtree (1 for a file)
    pub files: u64,
    /// folders in the subtree, excluding the node itself
    pub dirs: u64,
    pub mtime: i64,
    flags: u8,
    link: Option<(u64, u64)>,
}

impl Node {
    pub fn bytes(&self, m: Mode) -> u64 {
        match m {
            Mode::Alloc => self.alloc,
            Mode::Apparent => self.size,
        }
    }
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
    pub fn file_kind(&self) -> Option<Kind> {
        match self.kind {
            NodeKind::File(k) => Some(k),
            _ => None,
        }
    }
    pub fn unreadable(&self) -> bool {
        self.flags & UNREADABLE != 0
    }
    pub fn removed(&self) -> bool {
        self.flags & REMOVED != 0
    }
}

/// Signed change of a subtree's totals, applied to every ancestor.
#[derive(Debug, Clone, Copy, Default)]
struct Delta {
    size: i64,
    alloc: i64,
    files: i64,
    dirs: i64,
}

impl Delta {
    fn of(n: &Node) -> Self {
        Delta {
            size: n.size as i64,
            alloc: n.alloc as i64,
            files: n.files as i64,
            dirs: n.dirs as i64 + i64::from(n.is_dir() || n.kind == NodeKind::OtherFs),
        }
    }
    fn neg(self) -> Self {
        Delta {
            size: -self.size,
            alloc: -self.alloc,
            files: -self.files,
            dirs: -self.dirs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Size,
    Name,
    Files,
    Modified,
}

#[derive(Debug, Clone)]
pub struct ExtStat {
    pub ext: String,
    pub kind: Kind,
    pub count: u64,
    pub bytes: u64,
}

/// A subtree to scan, produced by `apply_probe` for new directories.
#[derive(Debug, Clone)]
pub struct SubScan {
    pub attach: u32,
    pub path: PathBuf,
    pub name: Box<str>,
}

#[derive(Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root_path: PathBuf,
    pub unreadable: u64,
    /// hard-linked files already counted (dev, ino)
    links: HashSet<(u64, u64)>,
}

pub fn ext_of(name: &str) -> String {
    ext_slice(name).to_ascii_lowercase()
}

/// Extension as `Path::extension` sees it, without allocating.
fn ext_slice(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i + 1..],
    }
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            nodes: Vec::new(),
            root_path,
            unreadable: 0,
            links: HashSet::new(),
        }
    }

    pub fn root(&self) -> Option<&Node> {
        self.nodes.first()
    }

    pub fn live(&self, id: u32) -> bool {
        self.nodes.get(id as usize).is_some_and(|n| !n.removed())
    }

    pub fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    fn add_up(&mut self, mut id: u32, d: Delta) {
        while id != NONE {
            let n = &mut self.nodes[id as usize];
            n.size = n.size.saturating_add_signed(d.size);
            n.alloc = n.alloc.saturating_add_signed(d.alloc);
            n.files = n.files.saturating_add_signed(d.files);
            n.dirs = n.dirs.saturating_add_signed(d.dirs);
            id = n.parent;
        }
    }

    /// Append a scan batch. `map` translates the job's local ids (it grows
    /// by one per node); the job root hangs under `attach` (NONE = tree root).
    pub fn apply(&mut self, map: &mut Vec<u32>, attach: u32, batch: Batch) {
        self.nodes.reserve(batch.nodes.len());
        map.reserve(batch.nodes.len());
        for n in batch.nodes {
            let parent = if n.parent == NONE {
                attach
            } else {
                map[n.parent as usize]
            };
            let id = self.nodes.len() as u32;
            let file = matches!(n.kind, NodeKind::File(_));
            // a dead parent, or another name of a file already counted
            let dead = (parent != NONE && self.nodes[parent as usize].removed())
                || n.link.is_some_and(|l| !self.links.insert(l));
            self.nodes.push(Node {
                name: n.name,
                parent,
                children: Vec::new(),
                kind: n.kind,
                size: n.size,
                alloc: n.alloc,
                files: u64::from(file),
                dirs: 0,
                mtime: n.mtime,
                flags: if dead { REMOVED } else { 0 },
                link: if dead { None } else { n.link },
            });
            map.push(id);
            if parent != NONE && !dead {
                self.nodes[parent as usize].children.push(id);
                let d = Delta::of(&self.nodes[id as usize]);
                self.add_up(parent, d);
            }
        }
        for &u in &batch.unreadable {
            let n = &mut self.nodes[map[u as usize] as usize];
            if n.flags & (UNREADABLE | REMOVED) == 0 {
                n.flags |= UNREADABLE;
                self.unreadable += 1;
            }
        }
    }

    pub fn path_of(&self, id: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut i = id;
        while i != 0 && i != NONE {
            let n = &self.nodes[i as usize];
            parts.push(&*n.name);
            i = n.parent;
        }
        let mut p = self.root_path.clone();
        p.extend(parts.into_iter().rev());
        p
    }

    /// Path relative to the root (for compact display).
    pub fn rel_path(&self, id: u32) -> String {
        let p = self.path_of(id);
        match p.strip_prefix(&self.root_path) {
            Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
            _ => p.display().to_string(),
        }
    }

    pub fn child_named(&self, id: u32, name: &str) -> Option<u32> {
        self.nodes[id as usize]
            .children
            .iter()
            .copied()
            .find(|&c| &*self.nodes[c as usize].name == name)
    }

    pub fn find(&self, path: &Path) -> Option<u32> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        if self.nodes.is_empty() {
            return None;
        }
        let mut id = 0;
        for c in rel.components() {
            id = self.child_named(id, &c.as_os_str().to_string_lossy())?;
        }
        Some(id)
    }

    /// Ancestors of `id` from the root down to `id` itself.
    pub fn chain(&self, id: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut i = id;
        while i != NONE {
            v.push(i);
            i = self.nodes[i as usize].parent;
        }
        v.reverse();
        v
    }

    pub fn is_ancestor(&self, anc: u32, mut id: u32) -> bool {
        while id != NONE {
            if id == anc {
                return true;
            }
            id = self.nodes[id as usize].parent;
        }
        false
    }

    /// Drop a node (and its subtree) and take its totals off every ancestor.
    pub fn remove(&mut self, id: u32) {
        if !self.live(id) {
            return;
        }
        let d = Delta::of(&self.nodes[id as usize]).neg();
        let parent = self.nodes[id as usize].parent;
        if parent != NONE {
            self.nodes[parent as usize].children.retain(|&c| c != id);
            self.add_up(parent, d);
        }
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            let n = &mut self.nodes[i as usize];
            if n.flags & UNREADABLE != 0 {
                self.unreadable = self.unreadable.saturating_sub(1);
            }
            n.flags = REMOVED;
            if let Some(l) = n.link.take() {
                self.links.remove(&l);
            }
            stack.extend(std::mem::take(&mut n.children));
        }
    }

    fn add_entry(&mut self, parent: u32, e: &RawEntry) -> Option<u32> {
        let link = e.link();
        if link.is_some_and(|l| !self.links.insert(l)) {
            return None; // another name of a file already counted
        }
        let id = self.nodes.len() as u32;
        let file = matches!(e.kind, NodeKind::File(_));
        self.nodes.push(Node {
            name: e.name.clone(),
            parent,
            children: Vec::new(),
            kind: e.kind,
            size: e.size,
            alloc: e.alloc,
            files: u64::from(file),
            dirs: 0,
            mtime: e.mtime,
            flags: 0,
            link,
        });
        self.nodes[parent as usize].children.push(id);
        let d = Delta::of(&self.nodes[id as usize]);
        self.add_up(parent, d);
        Some(id)
    }

    /// Update a file node in place from a fresh stat.
    fn refresh_file(&mut self, id: u32, e: &RawEntry) {
        let n = &mut self.nodes[id as usize];
        let d = Delta {
            size: e.size as i64 - n.size as i64,
            alloc: e.alloc as i64 - n.alloc as i64,
            ..Delta::default()
        };
        n.size = e.size;
        n.alloc = e.alloc;
        n.mtime = e.mtime;
        n.kind = e.kind;
        let parent = n.parent;
        self.add_up(parent, d);
    }

    /// Reconcile one child entry under existing dir `dir`; new directories
    /// are returned for a background subtree scan.
    fn reconcile(&mut self, dir: u32, e: &RawEntry, subs: &mut Vec<SubScan>) {
        let existing = self.child_named(dir, &e.name);
        let same_kind = existing.is_some_and(|c| {
            let k = self.nodes[c as usize].kind;
            matches!(
                (k, e.kind),
                (NodeKind::Dir, NodeKind::Dir)
                    | (NodeKind::OtherFs, NodeKind::OtherFs)
                    | (NodeKind::File(_), NodeKind::File(_))
            )
        });
        match existing {
            Some(c) if same_kind => {
                if matches!(e.kind, NodeKind::File(_)) {
                    self.refresh_file(c, e);
                } else {
                    self.nodes[c as usize].mtime = e.mtime;
                }
            }
            _ => {
                if let Some(c) = existing {
                    self.remove(c);
                }
                if e.kind == NodeKind::Dir {
                    subs.push(SubScan {
                        attach: dir,
                        path: self.path_of(dir).join(&*e.name),
                        name: e.name.clone(),
                    });
                } else {
                    self.add_entry(dir, e);
                }
            }
        }
    }

    /// Apply what `scan::probe` found. Returns new directories to scan.
    pub fn apply_probe(&mut self, probes: &[Probe]) -> Vec<SubScan> {
        let mut subs = Vec::new();
        for p in probes {
            match p {
                Probe::Gone(path) => match self.find(path) {
                    Some(0) => {
                        // the analyzed folder itself is gone
                        let kids = self.nodes[0].children.clone();
                        for c in kids {
                            self.remove(c);
                        }
                    }
                    Some(id) => self.remove(id),
                    None => {}
                },
                Probe::File(path, e) => match self.find(path) {
                    Some(id) if id != 0 => {
                        let parent = self.nodes[id as usize].parent;
                        self.reconcile(parent, e, &mut subs);
                    }
                    Some(_) => {}
                    None => {
                        if let Some(parent) = path.parent().and_then(|pp| self.find(pp)) {
                            if self.nodes[parent as usize].is_dir() {
                                self.reconcile(parent, e, &mut subs);
                            }
                        }
                    }
                },
                Probe::Dir(path, e, entries, ok) => {
                    let Some(id) = self.find(path) else {
                        if let Some(parent) = path.parent().and_then(|pp| self.find(pp)) {
                            if self.nodes[parent as usize].is_dir() {
                                self.reconcile(parent, e, &mut subs);
                            }
                        }
                        continue;
                    };
                    if !self.nodes[id as usize].is_dir() {
                        if id != 0 {
                            let parent = self.nodes[id as usize].parent;
                            self.reconcile(parent, e, &mut subs);
                        }
                        continue;
                    }
                    if !ok {
                        continue;
                    }
                    self.nodes[id as usize].mtime = e.mtime;
                    let names: std::collections::HashSet<&str> =
                        entries.iter().map(|e| &*e.name).collect();
                    let stale: Vec<u32> = self.nodes[id as usize]
                        .children
                        .iter()
                        .copied()
                        .filter(|&c| !names.contains(&*self.nodes[c as usize].name))
                        .collect();
                    for c in stale {
                        self.remove(c);
                    }
                    for e in entries {
                        self.reconcile(id, e, &mut subs);
                    }
                }
            }
        }
        // a new dir already queued under a parent that's also new is
        // covered by the parent's scan
        subs.dedup_by(|a, b| a.path == b.path);
        subs
    }

    pub fn sorted_children(&self, id: u32, sort: Sort, mode: Mode) -> Vec<u32> {
        let mut v = self.nodes[id as usize].children.clone();
        let n = |i: &u32| &self.nodes[*i as usize];
        match sort {
            Sort::Size => {
                v.sort_unstable_by(|a, b| n(b).bytes(mode).cmp(&n(a).bytes(mode)).then(a.cmp(b)))
            }
            Sort::Name => v.sort_by(|a, b| {
                (!n(a).is_dir())
                    .cmp(&!n(b).is_dir())
                    .then_with(|| n(a).name.to_lowercase().cmp(&n(b).name.to_lowercase()))
            }),
            Sort::Files => v.sort_unstable_by(|a, b| n(b).files.cmp(&n(a).files).then(a.cmp(b))),
            Sort::Modified => v.sort_unstable_by(|a, b| n(b).mtime.cmp(&n(a).mtime).then(a.cmp(b))),
        }
        v
    }

    /// Visit every live file under `id`.
    fn each_file<'a>(&'a self, id: u32, mut f: impl FnMut(u32, &'a Node)) {
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i as usize];
            if n.file_kind().is_some() {
                f(i, n);
            }
            stack.extend_from_slice(&n.children);
        }
    }

    /// Files under `id` grouped by extension, biggest total first.
    pub fn ext_stats(&self, id: u32, mode: Mode) -> Vec<ExtStat> {
        // borrowed, case-sensitive keys in the hot loop (no allocation per
        // file); folded to lowercase once per distinct spelling afterwards
        let mut raw: HashMap<&str, (u64, u64, Kind)> = HashMap::new();
        self.each_file(id, |_, n| {
            let e = raw.entry(ext_slice(&n.name)).or_insert((
                0,
                0,
                n.file_kind().unwrap_or(Kind::Binary),
            ));
            e.0 += 1;
            e.1 += n.bytes(mode);
        });
        let mut m: HashMap<String, ExtStat> = HashMap::new();
        for (ext, (count, bytes, kind)) in raw {
            let e = m
                .entry(ext.to_ascii_lowercase())
                .or_insert_with_key(|k| ExtStat {
                    ext: k.clone(),
                    kind,
                    count: 0,
                    bytes: 0,
                });
            e.count += count;
            e.bytes += bytes;
        }
        let mut v: Vec<ExtStat> = m.into_values().collect();
        v.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.ext.cmp(&b.ext)));
        v
    }

    /// The `n` biggest files under `id`, biggest first.
    pub fn largest(&self, id: u32, n: usize, mode: Mode) -> Vec<u32> {
        let mut heap = BinaryHeap::new(); // min-heap via Reverse
        self.each_file(id, |i, node| {
            heap.push(std::cmp::Reverse((node.bytes(mode), i)));
            if heap.len() > n {
                heap.pop();
            }
        });
        let mut v: Vec<(u64, u32)> = heap.into_iter().map(|r| r.0).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, i)| i).collect()
    }
}

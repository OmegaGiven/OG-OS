//! Scanner / tree / treemap tests on real temp trees under
//! `~/.cache/ogx-du-tests` (removed afterwards).

use super::scan::{self, NodeKind, ScanEnd, NONE};
use super::tree::{Mode, Tree};
use super::treemap::{self, squarify, R};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct TempTree(PathBuf);

impl TempTree {
    fn new(tag: &str) -> Self {
        let base = crate::config::home().join(".cache/ogx-du-tests");
        let p = base.join(format!("{tag}-{}", std::process::id()));
        assert!(p.starts_with(&base));
        if p.exists() {
            unlock_and_remove(&p);
        }
        std::fs::create_dir_all(&p).unwrap();
        TempTree(p)
    }
}

fn unlock_and_remove(p: &Path) {
    // restore permissions of any locked dirs first so removal works
    fn unlock(p: &Path) {
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    unlock(&e.path());
                }
            }
        }
    }
    unlock(p);
    let _ = std::fs::remove_dir_all(p);
}

impl Drop for TempTree {
    fn drop(&mut self) {
        unlock_and_remove(&self.0);
        let base = self.0.parent().unwrap();
        let _ = std::fs::remove_dir(base); // only if empty
    }
}

fn write(p: &Path, n: usize) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, vec![b'x'; n]).unwrap();
}

fn alloc_of(p: &Path) -> u64 {
    std::fs::symlink_metadata(p).unwrap().blocks() * 512
}

fn scan_into(tree: &mut Tree, path: &Path, attach: u32, dev: Option<u64>) -> ScanEnd {
    let mut map = Vec::new();
    let cancel = AtomicBool::new(false);
    let name = path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
        .into_boxed_str();
    scan::scan(path, name, dev, &cancel, |b| {
        tree.apply(&mut map, attach, b);
        true
    })
    .unwrap()
}

fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions
    unsafe { libc::geteuid() == 0 }
}

/// root/
///   a/one.txt (1000)  a/two.png (5000)  a/deep/x/y/z.bin (3000)
///   b/big.mp4 (100000)  b/hl  (hard link to a/two.png)
///   link -> b/big.mp4   linkdir -> b   empty.dat (0)
///   locked/secret.txt  (locked/ is mode 000)
fn build(t: &TempTree) {
    let r = &t.0;
    write(&r.join("a/one.txt"), 1000);
    write(&r.join("a/two.png"), 5000);
    write(&r.join("a/deep/x/y/z.bin"), 3000);
    write(&r.join("b/big.mp4"), 100_000);
    std::fs::hard_link(r.join("a/two.png"), r.join("b/hl")).unwrap();
    std::os::unix::fs::symlink("b/big.mp4", r.join("link")).unwrap();
    std::os::unix::fs::symlink("b", r.join("linkdir")).unwrap();
    write(&r.join("empty.dat"), 0);
    write(&r.join("locked/secret.txt"), 777);
    std::fs::set_permissions(r.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
}

#[test]
fn diskusage_scan_totals_links_unreadable() {
    let t = TempTree::new("totals");
    build(&t);
    let r = &t.0;
    let mut tree = Tree::new(r.clone());
    let end = scan_into(&mut tree, r, NONE, None);
    assert!(!end.cancelled);
    let root = tree.root().unwrap();

    // apparent: files + symlinks' own lengths ("b/big.mp4" = 9, "b" = 1);
    // the hard link counted once, the symlinks not followed
    assert_eq!(root.size, 1000 + 5000 + 3000 + 100_000 + 9 + 1);
    let expect_alloc: u64 = [
        "a/one.txt",
        "a/two.png",
        "a/deep/x/y/z.bin",
        "b/big.mp4",
        "link",
        "linkdir",
        "empty.dat",
    ]
    .iter()
    .map(|p| alloc_of(&r.join(p)))
    .sum();
    assert_eq!(root.alloc, expect_alloc);
    assert!(root.alloc >= 100_000);
    assert_eq!(root.files, 7, "hard link counted once");
    assert_eq!(root.dirs, 6); // a deep x y b locked

    // exactly one of the two hard-link names made it in
    let two = tree.find(&r.join("a/two.png")).is_some();
    let hl = tree.find(&r.join("b/hl")).is_some();
    assert!(two ^ hl);

    let linkdir = tree.find(&r.join("linkdir")).unwrap();
    assert!(matches!(tree.node(linkdir).kind, NodeKind::File(_)));
    assert!(
        tree.node(linkdir).children.is_empty(),
        "symlinked dir not entered"
    );

    let locked = tree.find(&r.join("locked")).unwrap();
    if !running_as_root() {
        assert_eq!(tree.unreadable, 1);
        assert!(tree.node(locked).unreadable());
        assert!(tree.node(locked).children.is_empty());
    }

    // aggregates per folder
    let a = tree.find(&r.join("a")).unwrap();
    assert_eq!(tree.node(a).files, if two { 3 } else { 2 });
    let deep = tree.find(&r.join("a/deep")).unwrap();
    assert_eq!(tree.node(deep).size, 3000);
    assert_eq!(tree.node(deep).dirs, 2);
    assert_eq!(tree.path_of(deep), r.join("a/deep"));
    assert!(end
        .space
        .is_some_and(|(free, total)| total > 0 && free <= total));
}

#[test]
fn diskusage_ext_and_largest() {
    let t = TempTree::new("ext");
    build(&t);
    let r = &t.0;
    let mut tree = Tree::new(r.clone());
    scan_into(&mut tree, r, NONE, None);
    let exts = tree.ext_stats(0, Mode::Apparent);
    assert_eq!(exts[0].ext, "mp4");
    assert_eq!(exts[0].bytes, 100_000);
    assert_eq!(exts[0].count, 1);
    let png = exts.iter().find(|e| e.ext == "png");
    let none = exts.iter().find(|e| e.ext.is_empty()).unwrap();
    // "link", "linkdir" (+ "hl" if that name won the hard-link race)
    if png.is_some() {
        assert_eq!(none.count, 2);
        assert_eq!(none.bytes, 10);
    } else {
        assert_eq!(none.count, 3);
        assert_eq!(none.bytes, 5010);
    }
    let total: u64 = exts.iter().map(|e| e.bytes).sum();
    assert_eq!(total, tree.root().unwrap().size);
    assert!(exts.windows(2).all(|w| w[0].bytes >= w[1].bytes));

    let top = tree.largest(0, 3, Mode::Apparent);
    let sizes: Vec<u64> = top.iter().map(|&i| tree.node(i).size).collect();
    assert_eq!(sizes, vec![100_000, 5000, 3000]);
    assert_eq!(&*tree.node(top[0]).name, "big.mp4");
    // scoped to a focus folder
    let a = tree.find(&r.join("a/deep")).unwrap();
    let top = tree.largest(a, 100, Mode::Apparent);
    assert_eq!(top.len(), 1);
    assert_eq!(&*tree.node(top[0]).name, "z.bin");
}

#[test]
fn diskusage_fs_changed_remove_and_rescan() {
    let t = TempTree::new("changed");
    build(&t);
    let r = &t.0;
    let mut tree = Tree::new(r.clone());
    let end = scan_into(&mut tree, r, NONE, None);
    let a = tree.find(&r.join("a")).unwrap();
    let (root0, a0) = (tree.root().unwrap().clone(), tree.node(a).clone());

    // a file is removed (as a trash job would) → node gone, ancestors updated
    let gone = r.join("a/one.txt");
    std::fs::remove_file(&gone).unwrap();
    let probes = scan::probe(&[gone.clone(), r.join("a")], end.root_dev);
    let subs = tree.apply_probe(&probes);
    assert!(subs.is_empty());
    assert!(tree.find(&gone).is_none());
    assert_eq!(tree.node(a).size, a0.size - 1000);
    assert_eq!(tree.node(a).files, a0.files - 1);
    assert_eq!(tree.root().unwrap().size, root0.size - 1000);
    assert_eq!(tree.root().unwrap().files, root0.files - 1);

    // a removed folder takes its whole subtree off
    let deep = r.join("a/deep");
    std::fs::remove_dir_all(&deep).unwrap();
    tree.apply_probe(&scan::probe(&[deep.clone()], end.root_dev));
    assert!(tree.find(&deep).is_none());
    assert_eq!(tree.root().unwrap().size, root0.size - 4000);
    assert_eq!(tree.root().unwrap().dirs, root0.dirs - 3);

    // the folder changed: a new file + a new subfolder → shallow rescan adds
    // the file and hands back the subfolder for a background subtree scan
    write(&r.join("a/new.txt"), 700);
    write(&r.join("a/newdir/f1.bin"), 2000);
    write(&r.join("a/newdir/g/f2.bin"), 300);
    let subs = tree.apply_probe(&scan::probe(&[r.join("a")], end.root_dev));
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].attach, a);
    assert_eq!(subs[0].path, r.join("a/newdir"));
    scan_into(&mut tree, &subs[0].path, subs[0].attach, Some(end.root_dev));
    assert!(tree.find(&r.join("a/newdir/g/f2.bin")).is_some());
    assert_eq!(tree.root().unwrap().size, root0.size - 4000 + 3000);
    assert_eq!(tree.node(a).size, a0.size - 4000 + 3000);
    assert_eq!(tree.root().unwrap().files, root0.files - 2 + 3);

    // a file grew in place
    write(&r.join("a/new.txt"), 1700);
    tree.apply_probe(&scan::probe(&[r.join("a/new.txt")], end.root_dev));
    assert_eq!(tree.root().unwrap().size, root0.size - 4000 + 4000);
}

#[test]
fn diskusage_cancel_stops_quickly() {
    let t = TempTree::new("cancel");
    for i in 0..300 {
        for j in 0..8 {
            std::fs::create_dir_all(t.0.join(format!("d{i}/e{j}"))).unwrap();
        }
    }
    // already cancelled: returns at once, flagged, nothing past the root
    let cancel = AtomicBool::new(true);
    let mut nodes = 0;
    let t0 = Instant::now();
    let end = scan::scan(&t.0, "x".into(), None, &cancel, |b| {
        nodes += b.nodes.len();
        true
    })
    .unwrap();
    assert!(end.cancelled);
    assert_eq!(nodes, 0);
    assert!(t0.elapsed() < Duration::from_millis(500));

    // cancelled mid-flight from another thread
    let cancel = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_millis(3));
            cancel.store(true, Ordering::Relaxed);
        });
        let end = scan::scan(&t.0, "x".into(), None, &cancel, |_| true).unwrap();
        // either finished before the flag or stopped promptly after it
        assert!(
            end.elapsed < Duration::from_millis(1000),
            "{:?}",
            end.elapsed
        );
    });

    // receiver gone → scan cancels itself
    let cancel = AtomicBool::new(false);
    let end = scan::scan(&t.0, "x".into(), None, &cancel, |_| false).unwrap();
    assert!(end.elapsed < Duration::from_secs(2));
}

// ------------------------------------------------------------- treemap

fn lcg(seed: &mut u64) -> f64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*seed >> 33) as f64) / (1u64 << 31) as f64
}

fn check_tiling(sizes: &[f64], area: R) -> Vec<R> {
    let rects = squarify(sizes, area);
    let total: f64 = sizes.iter().sum();
    let mut sum = 0.0;
    for (s, r) in sizes.iter().zip(&rects) {
        assert!(r.w >= -1e-9 && r.h >= -1e-9);
        assert!(r.x >= area.x - 1e-6 && r.y >= area.y - 1e-6);
        assert!(r.x + r.w <= area.x + area.w + 1e-6);
        assert!(r.y + r.h <= area.y + area.h + 1e-6);
        let want = s / total * area.w * area.h;
        assert!(
            (r.w * r.h - want).abs() < 1.0,
            "area {} vs {want}",
            r.w * r.h
        );
        sum += r.w * r.h;
    }
    assert!(
        (sum - area.w * area.h).abs() < 1e-6,
        "tiles the area exactly"
    );
    for i in 0..rects.len() {
        for j in i + 1..rects.len() {
            let (a, b) = (rects[i], rects[j]);
            let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            assert!(ox <= 1e-6 || oy <= 1e-6, "rects {i} and {j} overlap");
        }
    }
    rects
}

fn aspect(r: &R) -> f64 {
    r.w.max(r.h) / r.w.min(r.h)
}

#[test]
fn diskusage_squarify_invariants() {
    // equal sizes in a square: near-perfect squares
    let rects = check_tiling(
        &[1.0; 16],
        R {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 400.0,
        },
    );
    assert!(rects.iter().all(|r| aspect(r) < 1.6));

    // the classic example from the paper
    let rects = check_tiling(
        &[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0],
        R {
            x: 0.0,
            y: 0.0,
            w: 6.0,
            h: 4.0,
        },
    );
    assert!(rects.iter().all(|r| aspect(r) < 3.0));

    // random distributions, offset rects, wide and tall areas
    let mut seed = 42;
    for (w, h) in [(800.0, 600.0), (1200.0, 90.0), (64.0, 900.0)] {
        for n in [1, 2, 5, 40, 120] {
            let mut sizes: Vec<f64> = (0..n).map(|_| 1.0 + lcg(&mut seed) * 99.0).collect();
            sizes.sort_by(|a, b| b.partial_cmp(a).unwrap());
            let rects = check_tiling(
                &sizes,
                R {
                    x: 13.0,
                    y: 7.0,
                    w,
                    h,
                },
            );
            let mut asp: Vec<f64> = rects.iter().map(aspect).collect();
            asp.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = asp[asp.len() / 2];
            // few items in a very wide strip can't be square; many can
            if n >= 40 {
                assert!(median < 2.5, "median aspect {median} for {n} in {w}x{h}");
            }
        }
    }
    // degenerate inputs
    assert!(squarify(
        &[],
        R {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0
        }
    )
    .is_empty());
    assert_eq!(
        squarify(
            &[5.0],
            R {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 4.0
            }
        )[0],
        R {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 4.0
        }
    );
}

#[test]
fn diskusage_layout_nests_and_hits() {
    let t = TempTree::new("layout");
    build(&t);
    let r = &t.0;
    let mut tree = Tree::new(r.clone());
    scan_into(&mut tree, r, NONE, None);
    let size = iced::Size::new(800.0, 600.0);
    let rects = treemap::layout(&tree, 0, size, Mode::Apparent);
    // files only are painted leaves; folders nest ≥ 3 levels here
    assert!(rects.iter().any(|l| l.depth >= 3));
    let z = tree.find(&r.join("a/deep/x/y/z.bin")).unwrap();
    let lz = rects.iter().find(|l| l.node == z).expect("z.bin placed");
    assert!(lz.leaf);
    assert_eq!(treemap::hit(&rects, lz.r.center()).unwrap().node, z);
    // the biggest file gets ~its share of the area (minus nesting borders)
    let big = tree.find(&r.join("b/big.mp4")).unwrap();
    let lb = rects.iter().find(|l| l.node == big).unwrap();
    let share = lb.r.width * lb.r.height / (800.0 * 600.0);
    let want = 100_000.0 / tree.root().unwrap().size as f32;
    assert!((share - want).abs() < 0.02, "{share} vs {want}");
    // every rect lies inside its parent's rect
    for l in &rects {
        let p = tree.node(l.node).parent;
        if let Some(pr) = rects.iter().find(|x| x.node == p) {
            assert!(l.r.x >= pr.r.x - 0.01 && l.r.y >= pr.r.y - 0.01);
            assert!(l.r.x + l.r.width <= pr.r.x + pr.r.width + 0.01);
            assert!(l.r.y + l.r.height <= pr.r.y + pr.r.height + 0.01);
        }
    }
}

/// Read-only throughput check on a real tree:
/// `DU_BENCH=/usr cargo test -p og-files diskusage_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn diskusage_bench() {
    let root = PathBuf::from(std::env::var("DU_BENCH").unwrap_or_else(|_| "/usr".into()));
    let mut tree = Tree::new(root.clone());
    let mut batches = 0;
    let mut apply = Duration::ZERO;
    let mut map = Vec::new();
    let cancel = AtomicBool::new(false);
    let end = scan::scan(&root, "bench".into(), None, &cancel, |b| {
        let t = Instant::now();
        tree.apply(&mut map, NONE, b);
        apply += t.elapsed();
        batches += 1;
        true
    })
    .unwrap();
    let r = tree.root().unwrap();
    let t = Instant::now();
    let rects = treemap::layout(&tree, 0, iced::Size::new(1600.0, 1000.0), Mode::Alloc);
    let lay = t.elapsed();
    let t = Instant::now();
    let exts = tree.ext_stats(0, Mode::Alloc);
    let top = tree.largest(0, 100, Mode::Alloc);
    let aggr = t.elapsed();
    println!(
        "{}: {} files, {} dirs, {} nodes, {:.2} GiB alloc / {:.2} GiB apparent, {} unreadable \
         in {:.2?} ({:.0} entries/s), {batches} batches, apply total {apply:.2?}; \
         layout {} rects in {lay:.2?}; {} exts + top {} in {aggr:.2?}",
        root.display(),
        r.files,
        r.dirs,
        tree.nodes.len(),
        r.alloc as f64 / (1u64 << 30) as f64,
        r.size as f64 / (1u64 << 30) as f64,
        tree.unreadable,
        end.elapsed,
        tree.nodes.len() as f64 / end.elapsed.as_secs_f64(),
        rects.len(),
        exts.len(),
        top.len(),
    );
}

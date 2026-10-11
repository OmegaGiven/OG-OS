//! Page-range expressions, 1-based for humans, 0-based out.
//!
//! `all` · `odd` · `even` · `5` · `1-3` · `10-` (to the end) · `-4` (from the
//! start) · `last` · comma-separated mixes, e.g. `1-3,7,10-`. Order is kept
//! and duplicates removed.

pub fn parse(spec: &str, page_count: usize) -> Result<Vec<usize>, String> {
    if page_count == 0 {
        return Err("document has no pages".into());
    }
    let spec = spec.trim().to_ascii_lowercase();
    if spec.is_empty() || spec == "all" {
        return Ok((0..page_count).collect());
    }
    let mut out = Vec::new();
    let mut seen = vec![false; page_count];
    let mut push = |p: usize, out: &mut Vec<usize>| {
        if !seen[p] {
            seen[p] = true;
            out.push(p);
        }
    };
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part {
            "odd" => (0..page_count).step_by(2).for_each(|p| push(p, &mut out)),
            "even" => (1..page_count).step_by(2).for_each(|p| push(p, &mut out)),
            "last" => push(page_count - 1, &mut out),
            _ => {
                let num = |s: &str| -> Result<usize, String> {
                    let s = s.trim();
                    if s == "last" {
                        return Ok(page_count);
                    }
                    s.parse::<usize>()
                        .map_err(|_| format!("'{part}' isn't a page or range"))
                };
                let (a, b) = match part.split_once('-') {
                    Some((a, b)) => (
                        if a.trim().is_empty() { 1 } else { num(a)? },
                        if b.trim().is_empty() {
                            page_count
                        } else {
                            num(b)?
                        },
                    ),
                    None => {
                        let n = num(part)?;
                        (n, n)
                    }
                };
                if a == 0 || b == 0 || a > page_count || b > page_count {
                    return Err(format!("'{part}' is outside 1–{page_count}"));
                }
                if a <= b {
                    (a..=b).for_each(|p| push(p - 1, &mut out));
                } else {
                    (b..=a).rev().for_each(|p| push(p - 1, &mut out));
                }
            }
        }
    }
    if out.is_empty() {
        return Err("no pages selected".into());
    }
    Ok(out)
}

/// Comma-separated groups → one page list per group, for "split by ranges":
/// `1-3, 4-8, 9-` → three documents. A `;` also separates groups.
pub fn parse_groups(spec: &str, page_count: usize) -> Result<Vec<Vec<usize>>, String> {
    spec.split([',', ';'])
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(|g| parse(g, page_count))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn ranges() {
        assert_eq!(parse("all", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse("1-3,7,10-", 11).unwrap(), vec![0, 1, 2, 6, 9, 10]);
        assert_eq!(parse("odd", 5).unwrap(), vec![0, 2, 4]);
        assert_eq!(parse("even", 5).unwrap(), vec![1, 3]);
        assert_eq!(parse("-2,last", 5).unwrap(), vec![0, 1, 4]);
        assert_eq!(parse("3-1", 5).unwrap(), vec![2, 1, 0]);
        assert_eq!(parse("2,2,2", 5).unwrap(), vec![1]);
        assert!(parse("0", 5).is_err());
        assert!(parse("9", 5).is_err());
        assert!(parse("x", 5).is_err());
    }

    #[test]
    fn groups() {
        assert_eq!(
            super::parse_groups("1-2, 3-", 4).unwrap(),
            vec![vec![0, 1], vec![2, 3]]
        );
    }
}

//! A minimal unified diff over lines, for the save-conflict banner.
//!
//! The banner used to show the entire file on disk as `-` lines followed by
//! the entire buffer as `+` lines, which is not a diff: a two-line divergence
//! in `docs/backlog.md` rendered as 336 red lines above 347 green ones, and
//! the reader had to find the difference themselves. Autosave makes conflicts
//! both smaller and more frequent, so the banner has to answer "what
//! diverged" at a glance.
//!
//! Deliberately not `git diff`: a conflict compares what is on disk against
//! an *unsaved buffer*, which is not a blob git has ever seen, and shelling
//! out would mean writing the buffer to a temp file first — a write, on the
//! path whose entire job is to avoid an unwanted one.

/// Lines of unchanged context kept around each change.
const CONTEXT: usize = 3;

/// Cap on the divergent region, per side, before falling back to a summary.
/// The comparison is quadratic in this, and both inputs can be 2 MB
/// (`fileops::MAX_WRITE_BYTES`); a conflict on two files that share almost
/// nothing must not park a socket thread for minutes building a diff nobody
/// can read anyway.
const MAX_DIVERGENT_LINES: usize = 1_000;

pub fn unified(disk: &str, buf: &str) -> String {
    let a: Vec<&str> = disk.lines().collect();
    let b: Vec<&str> = buf.lines().collect();

    // Trimming the shared head and tail first is what keeps the quadratic
    // step off the whole file: the usual conflict is a handful of lines
    // inside two otherwise identical versions.
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let (mid_a, mid_b) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);

    let mut out = String::from("--- a/disk\n+++ b/your buffer\n");
    if mid_a.is_empty() && mid_b.is_empty() {
        // Reachable only if the hashes disagreed while the text did not,
        // which would be a bug elsewhere — say so rather than render nothing
        // and leave the banner looking like it lost the diff.
        out.push_str("@@ conflict @@\n the two versions are identical line for line\n");
        return out;
    }
    if mid_a.len() > MAX_DIVERGENT_LINES || mid_b.len() > MAX_DIVERGENT_LINES {
        out.push_str("@@ conflict @@\n");
        out.push_str(" the two versions are too different to show as a diff\n");
        out.push_str(&format!(
            " on disk: {} lines · your buffer: {} lines ({} and {} of them diverge)\n",
            a.len(),
            b.len(),
            mid_a.len(),
            mid_b.len(),
        ));
        return out;
    }

    let ops = align(mid_a, mid_b, pre);
    // Context can come from the trimmed head and tail, so hunk assembly works
    // over the whole file: every line is an op, with the trimmed regions
    // filled back in as equal ones.
    let mut all: Vec<Op> = (0..pre).map(|i| Op::Eq(i, i)).collect();
    all.extend(ops);
    let (ta, tb) = (a.len() - suf, b.len() - suf);
    all.extend((0..suf).map(|i| Op::Eq(ta + i, tb + i)));

    for (from, to) in hunks(&all) {
        let (mut sa, mut sb, mut ca, mut cb) = (usize::MAX, usize::MAX, 0, 0);
        for op in &all[from..to] {
            match *op {
                Op::Eq(i, j) => {
                    sa = sa.min(i);
                    sb = sb.min(j);
                    ca += 1;
                    cb += 1;
                }
                Op::Del(i) => {
                    sa = sa.min(i);
                    ca += 1;
                }
                Op::Ins(j) => {
                    sb = sb.min(j);
                    cb += 1;
                }
            }
        }
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            sa.saturating_add(1),
            ca,
            sb.saturating_add(1),
            cb
        ));
        for op in &all[from..to] {
            match *op {
                Op::Eq(i, _) => out.push_str(&format!(" {}\n", a[i])),
                Op::Del(i) => out.push_str(&format!("-{}\n", a[i])),
                Op::Ins(j) => out.push_str(&format!("+{}\n", b[j])),
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    /// Indices into the disk and buffer line vectors, always in step.
    Eq(usize, usize),
    Del(usize),
    Ins(usize),
}

impl Op {
    fn is_change(self) -> bool {
        !matches!(self, Op::Eq(..))
    }
}

/// Longest common subsequence over the divergent middle, walked back into an
/// op per line. `offset` shifts the indices back onto the whole file.
fn align(a: &[&str], b: &[&str], offset: usize) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[at(i, j)] = if a[i] == b[j] {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::with_capacity(n + m));
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(Op::Eq(offset + i, offset + j));
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            out.push(Op::Del(offset + i));
            i += 1;
        } else {
            out.push(Op::Ins(offset + j));
            j += 1;
        }
    }
    // Deletions before insertions in the tail, so a replaced block reads as
    // the old lines then the new ones rather than interleaved.
    out.extend((i..n).map(|i| Op::Del(offset + i)));
    out.extend((j..m).map(|j| Op::Ins(offset + j)));
    out
}

/// Ranges of `ops` worth printing: every change, plus CONTEXT lines around
/// it, with neighbours merged when their context would overlap.
fn hunks(ops: &[Op]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if !op.is_change() {
            continue;
        }
        let from = i.saturating_sub(CONTEXT);
        let to = (i + CONTEXT + 1).min(ops.len());
        match out.last_mut() {
            Some(last) if from <= last.1 => last.1 = to,
            _ => out.push((from, to)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    /// The defect this module exists for: one changed line in a 60-line file
    /// used to print all 120 lines. The negative assertions are the real
    /// test — a whole-file dump satisfies every positive one.
    #[test]
    fn one_changed_line_shows_that_line_and_its_context_only() {
        let disk = numbered(60);
        let buf = disk.replace("line 30\n", "line 30 edited\n");
        let d = unified(&disk, &buf);
        assert!(d.contains("-line 30\n"), "the disk's version of the changed line: {d}");
        assert!(d.contains("+line 30 edited\n"), "the buffer's version: {d}");
        assert!(d.contains(" line 29\n") && d.contains(" line 31\n"), "context on both sides: {d}");
        assert!(!d.contains(" line 1\n"), "a line 29 away must not be dumped: {d}");
        assert!(!d.contains(" line 60\n"), "nor one 30 away: {d}");
    }

    /// Two separate edits are two hunks, not one spanning everything between
    /// them — the shape `docs/backlog.md` actually had.
    #[test]
    fn distant_changes_become_separate_hunks() {
        let disk = numbered(80);
        let buf = disk.replace("line 10\n", "line 10 edited\n").replace("line 70\n", "");
        let d = unified(&disk, &buf);
        let headers = d.lines().filter(|l| l.starts_with("@@")).count();
        assert_eq!(headers, 2, "one header per hunk, two hunks: {d}");
        assert!(!d.contains(" line 40\n"), "the untouched middle must not be included: {d}");
    }

    /// Line numbers are the only way to find the change in the real file, and
    /// a diff that prints them wrongly is worse than one that omits them.
    #[test]
    fn hunk_headers_carry_the_line_numbers() {
        let disk = numbered(20);
        let buf = disk.replace("line 12\n", "line 12 edited\n");
        let d = unified(&disk, &buf);
        // 3 lines of context before line 12 => the hunk starts at line 9, and
        // covers 3 + 1 + 3 lines on each side.
        assert!(d.contains("@@ -9,7 +9,7 @@"), "expected a hunk header at 9,7: {d}");
    }

    /// Trailing "\ No newline" pedantry is out of scope, but a file with no
    /// final newline must still diff rather than panic or lose its last line.
    #[test]
    fn a_missing_final_newline_is_not_a_panic() {
        let d = unified("a\nb", "a\nc");
        assert!(d.contains("-b") && d.contains("+c"), "got {d}");
    }

    /// Two files that share nothing would be quadratic to align and useless
    /// to read. The fallback must say so rather than silently truncate —
    /// "the diff is not shown" and "there is no difference" must not look
    /// alike.
    #[test]
    fn a_wholly_divergent_pair_falls_back_to_a_summary() {
        let disk: String = (0..MAX_DIVERGENT_LINES + 50).map(|i| format!("disk {i}\n")).collect();
        let buf: String = (0..MAX_DIVERGENT_LINES + 50).map(|i| format!("buffer {i}\n")).collect();
        let d = unified(&disk, &buf);
        assert!(d.contains("too different to show"), "must say why it is not showing one: {d}");
        assert!(d.contains(&format!("{}", MAX_DIVERGENT_LINES + 50)), "with the sizes: {d}");
        assert!(d.lines().count() < 20, "and must not dump either file: {d}");
    }

    /// A pair that is *large but similar* is the common case for a big file:
    /// it must still get a real diff, or the fallback would swallow every
    /// conflict in any file over a thousand lines.
    #[test]
    fn a_large_but_similar_pair_still_gets_a_real_diff() {
        let disk = numbered(5_000);
        let buf = disk.replace("line 2500\n", "line 2500 edited\n");
        let d = unified(&disk, &buf);
        assert!(d.contains("+line 2500 edited\n"), "got {d}");
        assert!(d.lines().count() < 20, "one small hunk, not 10,000 lines: {d}");
    }
}


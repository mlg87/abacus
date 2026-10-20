use std::fmt::Write;
use std::path::PathBuf;

use crate::runs::Mode;

#[derive(Debug, Clone, Default)]
pub struct BeadRef {
    pub id: String,
    pub title: String,
    /// Status glyph or label shown next to the id.
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct PreviousRun {
    pub run_id: String,
    pub state: String,
    pub exit_code: Option<i32>,
    pub result_line: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PromptCtx {
    pub repo_name: String,
    pub repo_root: PathBuf,
    pub id: String,
    pub issue_type: String,
    pub priority: i32,
    pub title: String,
    pub labels: Vec<String>,
    pub description: String,
    pub acceptance_criteria: String,
    pub notes: String,
    pub parent: Option<BeadRef>,
    pub blockers: Vec<BeadRef>,
    pub children: Vec<BeadRef>,
    pub mode: Mode,
    pub worktree: PathBuf,
    pub branch: Option<String>,
    pub base: String,
    /// GitHub issue number and URL from the bead itself or its nearest ancestor.
    pub gh: Option<(u32, Option<String>)>,
    /// Id of the bead that carries `gh` (self for resolve, an ancestor for implement).
    pub gh_bead_id: Option<String>,
    pub previous: Option<PreviousRun>,
}

pub fn render(c: &PromptCtx) -> String {
    let mut s = String::new();
    let checkout = match &c.branch {
        Some(b) => format!("on branch `{b}`"),
        None => format!("detached at `{}`", c.base),
    };
    let _ = writeln!(
        s,
        "You were dispatched by abacus to work bead `{id}` in `{repo}`. Your working directory `{wt}` is a git worktree ({checkout}) created from `{base}`. Do all work there. Never edit files, switch branches, or stash in the primary checkout `{root}`. Read and follow the repo's AGENTS.md and CLAUDE.md; they take precedence over this prompt except for the working directory.",
        id = c.id,
        repo = c.repo_name,
        wt = c.worktree.display(),
        base = c.base,
        root = c.repo_root.display(),
    );
    if c.mode != Mode::Decompose {
        let _ = writeln!(
            s,
            "\nBead `{}` is already claimed for you (in_progress).",
            c.id
        );
    }
    // A real run used `--notes` and erased abacus's claim note (v0.1.0 smoke run).
    let _ = writeln!(
        s,
        "\nRecord progress on beads with `bd update <id> --append-notes \"…\"`, never `--notes`, which replaces the run note abacus wrote."
    );

    let _ = writeln!(s, "\n## Bead\n");
    let _ = writeln!(s, "- id: `{}`", c.id);
    let _ = writeln!(s, "- type: {}", c.issue_type);
    let _ = writeln!(s, "- priority: P{}", c.priority);
    let _ = writeln!(s, "- title: {}", c.title);
    if !c.labels.is_empty() {
        let _ = writeln!(s, "- labels: {}", c.labels.join(", "));
    }
    if let Some((n, url)) = &c.gh {
        match url {
            Some(u) => {
                let _ = writeln!(s, "- GitHub issue: #{n} ({u})");
            }
            None => {
                let _ = writeln!(s, "- GitHub issue: #{n}");
            }
        }
    }
    if let Some(p) = &c.parent {
        let _ = writeln!(s, "- parent: `{}` {}", p.id, p.title);
    }
    if !c.blockers.is_empty() {
        let _ = writeln!(s, "- blocked by:");
        for b in &c.blockers {
            let _ = writeln!(s, "  - `{}` [{}] {}", b.id, b.status, b.title);
        }
    }
    if c.mode != Mode::Implement {
        if c.children.is_empty() {
            let _ = writeln!(s, "- existing children: none");
        } else {
            let _ = writeln!(s, "- existing children:");
            for ch in &c.children {
                let _ = writeln!(s, "  - {} `{}` {}", ch.status, ch.id, ch.title);
            }
        }
    }
    for (heading, body) in [
        ("Description", &c.description),
        ("Acceptance criteria", &c.acceptance_criteria),
        ("Notes", &c.notes),
    ] {
        if !body.trim().is_empty() {
            let _ = writeln!(s, "\n### {heading}\n\n{}", body.trim_end());
        }
    }

    match c.mode {
        Mode::Resolve => render_resolve(&mut s, c),
        Mode::Implement => render_implement(&mut s, c),
        Mode::Decompose => render_decompose(&mut s, c),
    }

    if let Some(p) = &c.previous {
        let exit = p
            .exit_code
            .map_or_else(|| "none".to_string(), |e| e.to_string());
        let _ = writeln!(
            s,
            "\n## Previous attempt\n\nRun `{}` ended {} (exit {exit}), last result: {}. Inspect `git status` and `git log` in this worktree and continue from where it stopped; do not redo committed work.",
            p.run_id,
            p.state,
            p.result_line.as_deref().unwrap_or("none"),
        );
    }

    let _ = writeln!(
        s,
        "\nEnd your final message with exactly one line: `RESULT: <PR URL or none> — <one-sentence outcome>`."
    );
    s
}

fn render_resolve(s: &mut String, c: &PromptCtx) {
    let n = c.gh.as_ref().map_or(0, |(n, _)| *n);
    let id = &c.id;
    let _ = writeln!(
        s,
        "\n## Task: deliver GitHub issue #{n} end to end\n
1. Run `gh issue view {n} --comments`.
2. If `bd children {id}` is empty, decompose first with `bd create --graph` (`parent_id: {id}`, `blocks` edges for ordering, a `## Acceptance Criteria` section in each description; run with `--dry-run` first).
3. Implement the children in dependency order. Claim each child with `bd update <child> --claim` before editing it. Independent children may run as parallel subagents in this worktree when their files are disjoint.
4. Every commit subject ends with the id of the child bead it implements, e.g. `(<child-id>)`.
5. Open ONE pull request for the issue with `Closes #{n}` in the body, and follow the repo's merge, CI, and watch rules.
6. Do not close `{id}`; it closes from GitHub state."
    );
}

fn render_implement(s: &mut String, c: &PromptCtx) {
    let id = &c.id;
    let create = match &c.parent {
        Some(p) => format!("bd create … --parent {}", p.id),
        None => "bd create …".to_string(),
    };
    let _ = writeln!(
        s,
        "\n## Task: implement bead {id}\n
1. Implement only this bead's scope. Record any follow-up as a new bead (`{create}`) instead of doing it.
2. Every commit subject ends with `({id})`."
    );
    match (&c.gh, &c.gh_bead_id) {
        (Some((n, _)), Some(gh_id)) => {
            let _ = writeln!(
                s,
                "3. Open a pull request. Its body uses `Refs #{n}`, or `Closes #{n}` when `bd children {gh_id}` shows every other child closed."
            );
        }
        _ => {
            let _ = writeln!(
                s,
                "3. Open a pull request whose body describes this bead; there is no GitHub issue to reference."
            );
        }
    }
    let _ = writeln!(s, "4. Follow the repo's rules for closing the bead.");
}

fn render_decompose(s: &mut String, c: &PromptCtx) {
    let id = &c.id;
    let issue = match &c.gh {
        Some((n, _)) => format!("GitHub issue #{n} (`gh issue view {n} --comments`)"),
        None => "the bead description (there is no GitHub issue)".into(),
    };
    let _ = writeln!(
        s,
        "\n## Task: decompose epic {id} into child beads (planning only)\n
1. This worktree is a throwaway detached checkout: do not edit files, commit, push, or create branches or PRs.
2. Read {issue} and the code it touches.
3. Write the plan graph JSON under `$TMPDIR` and create the children with `bd create --graph` under `parent_id: {id}`, using `blocks` edges for ordering and a `## Acceptance Criteria` section in each description. Run with `--dry-run` first.
4. Never duplicate the existing children listed above; add only the missing ones.
5. Follow the repo's beads guide if it has one, e.g. `.claude/guides/beads-workflow.md` § Bulk decomposition."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(mode: Mode) -> PromptCtx {
        PromptCtx {
            repo_name: "dlb".into(),
            repo_root: "/w/dlb".into(),
            id: "dlb-abc".into(),
            issue_type: "epic".into(),
            priority: 1,
            title: "Native scoring engine".into(),
            labels: vec![],
            description: "Do the thing.".into(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            parent: None,
            blockers: vec![],
            children: vec![],
            mode,
            worktree: "/w/dlb-1329-native-scoring-engine".into(),
            branch: Some("1329-native-scoring-engine".into()),
            base: "origin/main".into(),
            gh: None,
            gh_bead_id: None,
            previous: None,
        }
    }

    #[test]
    fn every_mode_forbids_replacing_bead_notes() {
        for mode in [Mode::Resolve, Mode::Implement, Mode::Decompose] {
            let p = render(&ctx(mode));
            assert!(p.contains("--append-notes"), "{mode:?}");
            assert!(p.contains("never `--notes`"), "{mode:?}");
        }
    }

    #[test]
    fn resolve_closes_its_issue() {
        let mut c = ctx(Mode::Resolve);
        c.gh = Some((
            1329,
            Some("https://github.com/mlg87/dlb/issues/1329".into()),
        ));
        c.gh_bead_id = Some(c.id.clone());
        let p = render(&c);
        assert!(p.contains("Closes #1329"));
        assert!(p.contains("already claimed"));
        assert!(
            p.trim_end()
                .ends_with("`RESULT: <PR URL or none> — <one-sentence outcome>`.")
        );
    }

    #[test]
    fn implement_without_gh_ancestor_references_no_issue() {
        let mut c = ctx(Mode::Implement);
        c.issue_type = "task".into();
        let p = render(&c);
        assert!(!regex::Regex::new(r"#\d").unwrap().is_match(&p), "{p}");
        assert!(p.contains("(dlb-abc)"));
    }

    #[test]
    fn retry_includes_previous_attempt() {
        let mut c = ctx(Mode::Implement);
        c.previous = Some(PreviousRun {
            run_id: "1-dlb-abc".into(),
            state: "failed".into(),
            exit_code: Some(3),
            result_line: None,
        });
        let p = render(&c);
        assert!(p.contains("## Previous attempt"));
        assert!(p.contains("Run `1-dlb-abc` ended failed (exit 3), last result: none."));
    }

    #[test]
    fn decompose_forbids_commits_and_has_no_claim() {
        let p = render(&ctx(Mode::Decompose));
        assert!(p.contains("planning only"));
        assert!(!p.contains("already claimed"));
        assert!(p.contains("detached at") || p.contains("on branch"));
    }
}

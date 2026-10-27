use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use ratatui::style::Color;
use serde::{Deserialize, Deserializer};

/// `bd` omits optional fields instead of emitting null, but older rows have been
/// seen with explicit nulls; both collapse to the type's default.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub depends_on_id: String,
    #[serde(rename = "type")]
    pub dep_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bead {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub status: Status,
    #[serde(default, deserialize_with = "null_default")]
    pub priority: i32,
    #[serde(default, deserialize_with = "null_default")]
    pub issue_type: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub dependencies: Vec<Dependency>,
    #[serde(default, deserialize_with = "null_default")]
    pub notes: String,
    #[serde(default, deserialize_with = "null_default")]
    pub acceptance_criteria: String,
    #[serde(default, deserialize_with = "null_default")]
    pub created_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub close_reason: Option<String>,
}

/// Status as the user cares about it. Stricter than `bd ready`, which only checks a
/// bead's own `blocks` edges and so reports an epic as ready while every bead under
/// it is still waiting on upstream work; dispatching against such an epic is wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derived {
    Ready,
    Blocked,
    InProgress,
    Deferred,
    Closed,
    Other,
}

impl Derived {
    pub fn glyph(self) -> &'static str {
        match self {
            Derived::Ready => "○",
            Derived::Blocked => "●",
            Derived::InProgress => "◐",
            Derived::Deferred => "❄",
            Derived::Closed => "✓",
            Derived::Other => "?",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Derived::Ready => Color::Green,
            Derived::Blocked => Color::Red,
            Derived::InProgress => Color::Yellow,
            Derived::Deferred => Color::Blue,
            Derived::Closed => Color::DarkGray,
            Derived::Other => Color::Magenta,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Derived::Ready => "ready",
            Derived::Blocked => "blocked",
            Derived::InProgress => "in progress",
            Derived::Deferred => "deferred",
            Derived::Closed => "closed",
            Derived::Other => "other",
        }
    }
}

/// Why a bead derives as `Blocked`, so the UI and dispatch can name the cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedBy {
    /// The bead's own status is `blocked`.
    Status,
    /// Open `blocks` targets of the bead itself.
    Beads(Vec<usize>),
    /// Nearest ancestor that is itself gated by status or open blockers; its
    /// dependencies hold back everything beneath it.
    Ancestor(usize),
    /// The bead has open children but none of them is ready or in progress.
    NoReadyChild,
}

/// Parses the `gh-<N>` external ref the GitHub mirror stamps on epic beads.
pub fn gh_number(b: &Bead) -> Option<u32> {
    let digits = b.external_ref.as_deref()?.strip_prefix("gh-")?;
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[derive(Debug, Default)]
pub struct Graph {
    pub beads: Vec<Bead>,
    pub index: HashMap<String, usize>,
    pub parent: Vec<Option<usize>>,
    pub children: Vec<Vec<usize>>,
    pub blockers: Vec<Vec<usize>>,
    pub dependents: Vec<Vec<usize>>,
    pub other_links: Vec<Vec<(String, usize)>>,
    pub roots: Vec<usize>,
    derived: Vec<Derived>,
}

impl Graph {
    pub fn build(beads: Vec<Bead>) -> Graph {
        let n = beads.len();
        let index: HashMap<String, usize> = beads
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        let mut g = Graph {
            index,
            parent: vec![None; n],
            children: vec![Vec::new(); n],
            blockers: vec![Vec::new(); n],
            dependents: vec![Vec::new(); n],
            other_links: vec![Vec::new(); n],
            roots: Vec::new(),
            beads,
            derived: vec![Derived::Other; n],
        };

        for i in 0..n {
            let b = &g.beads[i];
            let from_field = b.parent.as_ref().and_then(|p| g.index.get(p).copied());
            let from_dep = || {
                b.dependencies
                    .iter()
                    .filter(|d| d.dep_type == "parent-child")
                    .find_map(|d| g.index.get(&d.depends_on_id).copied())
            };
            g.parent[i] = from_field.or_else(from_dep).filter(|&p| p != i);

            for d in &b.dependencies {
                let Some(&j) = g.index.get(&d.depends_on_id) else {
                    continue;
                };
                if j == i {
                    continue;
                }
                match d.dep_type.as_str() {
                    "parent-child" => {}
                    "blocks" => {
                        if !g.blockers[i].contains(&j) {
                            g.blockers[i].push(j);
                            g.dependents[j].push(i);
                        }
                    }
                    other => g.other_links[i].push((other.to_string(), j)),
                }
            }
        }

        g.break_parent_cycles();
        for i in 0..n {
            match g.parent[i] {
                Some(p) => g.children[p].push(i),
                None => g.roots.push(i),
            }
        }
        g.derive_all();

        // A `blocks` edge orders beads only among siblings, so each open blocker is
        // lifted to the pair of siblings holding it and its dependent. A closed
        // blocker no longer holds anything back, so it contributes no constraint.
        let mut lifted: HashMap<Option<usize>, Vec<(usize, usize)>> = HashMap::new();
        for d in 0..n {
            for &b in &g.blockers[d] {
                if g.beads[b].status == Status::Closed {
                    continue;
                }
                if let Some((x, y)) = g.sibling_pair(b, d) {
                    lifted.entry(g.parent[x]).or_default().push((x, y));
                }
            }
        }
        let mut pos = vec![0; n];
        let mut children = std::mem::take(&mut g.children);
        for (p, c) in children.iter_mut().enumerate() {
            let edges = lifted.get(&Some(p)).map_or(&[][..], Vec::as_slice);
            g.order_siblings(c, edges, &mut pos);
        }
        g.children = children;
        let mut roots = std::mem::take(&mut g.roots);
        let edges = lifted.get(&None).map_or(&[][..], Vec::as_slice);
        g.order_siblings(&mut roots, edges, &mut pos);
        g.roots = roots;
        let mut blockers = std::mem::take(&mut g.blockers);
        for b in &mut blockers {
            g.sort(b);
        }
        g.blockers = blockers;
        let mut dependents = std::mem::take(&mut g.dependents);
        for d in &mut dependents {
            g.sort(d);
        }
        g.dependents = dependents;
        g
    }

    /// A parent cycle would make its members unreachable from any root and so
    /// invisible in the tree; cut each cycle at the node where it is detected.
    fn break_parent_cycles(&mut self) {
        let n = self.beads.len();
        // 0 = unvisited, 1 = on current walk, 2 = done
        let mut state = vec![0u8; n];
        for start in 0..n {
            let mut path = Vec::new();
            let mut cur = Some(start);
            while let Some(i) = cur {
                match state[i] {
                    2 => break,
                    1 => {
                        self.parent[i] = None;
                        break;
                    }
                    _ => {
                        state[i] = 1;
                        path.push(i);
                        cur = self.parent[i];
                    }
                }
            }
            for i in path {
                state[i] = 2;
            }
        }
    }

    fn derive_all(&mut self) {
        self.derived = self.derive_as(None);
    }

    /// Children must be derived before their parent (roll-up), and gating is read
    /// off ancestors' own edges, so a post-order walk of the parent forest suffices.
    /// `closing` (if any) reads as closed, so `unblock_count` can ask what closing
    /// it would change.
    fn derive_as(&self, closing: Option<usize>) -> Vec<Derived> {
        let mut derived = vec![Derived::Other; self.beads.len()];
        let mut stack: Vec<(usize, bool)> = self.roots.iter().map(|&r| (r, false)).collect();
        while let Some((i, children_done)) = stack.pop() {
            if children_done {
                derived[i] = self.derive_one(i, &derived, closing);
            } else {
                stack.push((i, true));
                stack.extend(self.children[i].iter().map(|&c| (c, false)));
            }
        }
        derived
    }

    fn derive_one(&self, i: usize, derived: &[Derived], closing: Option<usize>) -> Derived {
        match self.status_as(i, closing) {
            Status::Open => {}
            Status::InProgress => return Derived::InProgress,
            Status::Blocked => return Derived::Blocked,
            Status::Deferred => return Derived::Deferred,
            Status::Closed => return Derived::Closed,
            Status::Other => return Derived::Other,
        }
        if self.has_open_blocker(i, closing) || self.gated_ancestor(i, closing).is_some() {
            return Derived::Blocked;
        }
        let mut open_children = self.children[i]
            .iter()
            .map(|&c| derived[c])
            .filter(|&d| d != Derived::Closed)
            .peekable();
        if open_children.peek().is_none() {
            return Derived::Ready;
        }
        let mut any_in_progress = false;
        for d in open_children {
            match d {
                Derived::Ready => return Derived::Ready,
                Derived::InProgress => any_in_progress = true,
                _ => {}
            }
        }
        if any_in_progress {
            Derived::InProgress
        } else {
            Derived::Blocked
        }
    }

    fn status_as(&self, j: usize, closing: Option<usize>) -> Status {
        if closing == Some(j) {
            Status::Closed
        } else {
            self.beads[j].status
        }
    }

    fn has_open_blocker(&self, i: usize, closing: Option<usize>) -> bool {
        self.blockers[i]
            .iter()
            .any(|&j| self.status_as(j, closing) != Status::Closed)
    }

    fn gated_ancestor(&self, i: usize, closing: Option<usize>) -> Option<usize> {
        self.ancestors(i).into_iter().find(|&a| {
            self.status_as(a, closing) == Status::Blocked || self.has_open_blocker(a, closing)
        })
    }

    /// Other beads that closing ready bead `i` would unblock: `Blocked` now but not
    /// once it closes, so their ● turns ○ (or ◐) in the tree. Includes the subtree
    /// of an ancestor it gates and parents whose only hope was a child it blocks.
    /// Dispatch-all ranks equal-priority work by it (v0.5).
    pub fn unblock_count(&self, i: usize) -> usize {
        // Only `blocks` edges let a bead's status gate other beads. Without
        // dependents, closing a ready bead changes just its ancestors' roll-up,
        // and none of them is blocked while it is ready.
        if self.dependents[i].is_empty() && self.derived[i] == Derived::Ready {
            return 0;
        }
        let after = self.derive_as(Some(i));
        (0..self.beads.len())
            .filter(|&j| {
                j != i && self.derived[j] == Derived::Blocked && after[j] != Derived::Blocked
            })
            .count()
    }

    /// Siblings read in dependency order (blockers first), then priority, then age;
    /// closed beads sink. Status is deliberately not part of the key: grouping by it
    /// put a P1 below a P2 and buried priority under whatever state a bead was in.
    pub fn sort(&self, v: &mut [usize]) {
        v.sort_by(|&a, &b| self.sort_key(a).cmp(&self.sort_key(b)));
    }

    /// Closed beads last; everything else priority-first, then oldest, then id for a
    /// total order. Also used for blockers, dependents, and dispatch targets.
    pub fn sort_key(&self, i: usize) -> (bool, i32, &str, &str) {
        let b = &self.beads[i];
        (b.status == Status::Closed, b.priority, &b.created_at, &b.id)
    }

    /// Orders `sibs` (children of one parent, or the roots) so that every bead follows
    /// the siblings that block it. Among the available siblings the smallest sort key
    /// goes next, so priority and age decide everything dependencies do not force.
    /// If a lifted blocks cycle leaves nothing available, the smallest remaining
    /// sibling is emitted anyway: every sibling appears exactly once and the walk
    /// always terminates. `pos` is scratch space sized to the bead count.
    fn order_siblings(&self, sibs: &mut [usize], lifted: &[(usize, usize)], pos: &mut [usize]) {
        self.sort(sibs);
        if lifted.is_empty() {
            return;
        }
        // Sorted position is the heap key, so "smallest sort key" is "smallest index".
        for (k, &s) in sibs.iter().enumerate() {
            pos[s] = k;
        }
        let m = sibs.len();
        let mut indeg = vec![0usize; m];
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); m];
        for &(b, d) in lifted {
            succ[pos[b]].push(pos[d]);
            indeg[pos[d]] += 1;
        }
        let mut ready: BinaryHeap<Reverse<usize>> =
            (0..m).filter(|&k| indeg[k] == 0).map(Reverse).collect();
        let mut done = vec![false; m];
        let mut out = Vec::with_capacity(m);
        let mut cursor = 0;
        loop {
            let k = match ready.pop() {
                Some(Reverse(k)) => k,
                None => {
                    while cursor < m && done[cursor] {
                        cursor += 1;
                    }
                    if cursor == m {
                        break;
                    }
                    cursor
                }
            };
            done[k] = true;
            out.push(sibs[k]);
            for &v in &succ[k] {
                if !done[v] {
                    indeg[v] -= 1;
                    if indeg[v] == 0 {
                        ready.push(Reverse(v));
                    }
                }
            }
        }
        sibs.copy_from_slice(&out);
    }

    /// The two siblings through which a `blocks` edge (blocker `b`, dependent `d`)
    /// constrains order: the children of their lowest common ancestor on each side,
    /// or the two roots when the trees are disjoint. `None` when one bead contains the
    /// other: an ancestor's own position already frames its descendants.
    fn sibling_pair(&self, b: usize, d: usize) -> Option<(usize, usize)> {
        let mut d_chain = vec![d];
        d_chain.extend(self.ancestors(d));
        let mut below_b = b;
        let mut cur = Some(b);
        while let Some(x) = cur {
            if let Some(at) = d_chain.iter().position(|&y| y == x) {
                if x == b || at == 0 {
                    return None;
                }
                return Some((below_b, d_chain[at - 1]));
            }
            below_b = x;
            cur = self.parent[x];
        }
        Some((below_b, *d_chain.last()?))
    }

    pub fn derived(&self, i: usize) -> Derived {
        self.derived[i]
    }

    /// `None` unless `i` derives as `Blocked`.
    pub fn blocked_by(&self, i: usize) -> Option<BlockedBy> {
        if self.derived[i] != Derived::Blocked {
            return None;
        }
        if self.beads[i].status == Status::Blocked {
            return Some(BlockedBy::Status);
        }
        let open = self.open_blockers(i);
        if !open.is_empty() {
            return Some(BlockedBy::Beads(open));
        }
        Some(match self.gated_ancestor(i, None) {
            Some(a) => BlockedBy::Ancestor(a),
            None => BlockedBy::NoReadyChild,
        })
    }

    /// Human-readable cause, shared by the detail pane and dispatch refusals.
    pub fn blocked_reason(&self, i: usize) -> Option<String> {
        Some(match self.blocked_by(i)? {
            BlockedBy::Status => "blocked (status)".into(),
            BlockedBy::Beads(js) => {
                let ids: Vec<&str> = js.iter().map(|&j| self.beads[j].id.as_str()).collect();
                format!("blocked by {}", ids.join(", "))
            }
            BlockedBy::Ancestor(a) => format!("blocked via ancestor {}", self.beads[a].id),
            BlockedBy::NoReadyChild => "blocked: no open child is ready".into(),
        })
    }

    pub fn get(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn is_epic(&self, i: usize) -> bool {
        self.beads[i].issue_type == "epic"
    }

    pub fn open_blockers(&self, i: usize) -> Vec<usize> {
        self.blockers[i]
            .iter()
            .copied()
            .filter(|&j| self.beads[j].status != Status::Closed)
            .collect()
    }

    /// Nearest bead (self first, then up the parent chain) carrying a GitHub issue number.
    pub fn gh_ancestor(&self, i: usize) -> Option<(usize, u32)> {
        std::iter::once(i)
            .chain(self.ancestors(i))
            .find_map(|j| gh_number(&self.beads[j]).map(|n| (j, n)))
    }

    /// Parent chain, nearest first.
    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([i]);
        let mut cur = self.parent[i];
        while let Some(p) = cur {
            if !seen.insert(p) {
                break;
            }
            out.push(p);
            cur = self.parent[p];
        }
        out
    }

    /// Every bead below `i`, in pre-order; excludes `i`.
    pub fn descendants(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::from([i]);
        let mut stack: Vec<usize> = self.children[i].iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            out.push(c);
            stack.extend(self.children[c].iter().rev().copied());
        }
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn bead(id: &str, status: &str) -> Bead {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("title {id}"), "status": status,
            "priority": 2, "issue_type": "task", "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    /// `_from` is the dependent bead; kept so call sites read as edges.
    pub fn dep(_from: &str, to: &str, ty: &str) -> Dependency {
        Dependency {
            depends_on_id: to.into(),
            dep_type: ty.into(),
        }
    }

    #[test]
    fn open_bead_is_blocked_until_its_blocker_closes() {
        let mut a = bead("a", "open");
        a.dependencies.push(dep("a", "b", "blocks"));
        let g = Graph::build(vec![a.clone(), bead("b", "open")]);
        assert_eq!(g.derived(g.get("a").unwrap()), Derived::Blocked);

        let g = Graph::build(vec![a, bead("b", "closed")]);
        assert_eq!(g.derived(g.get("a").unwrap()), Derived::Ready);
    }

    fn under(id: &str, status: &str, parent: &str) -> Bead {
        let mut b = bead(id, status);
        b.parent = Some(parent.into());
        b
    }

    fn blocked_on(mut b: Bead, on: &str) -> Bead {
        b.dependencies.push(dep(&b.id.clone(), on, "blocks"));
        b
    }

    fn with_priority(mut b: Bead, priority: i32, created_at: &str) -> Bead {
        b.priority = priority;
        b.created_at = created_at.into();
        b
    }

    /// The dispatch-against-unworkable-epic case: `bd ready` lists the epic because
    /// it has no blockers of its own, yet nothing beneath it can start.
    #[test]
    fn parent_whose_open_children_are_all_blocked_is_blocked() {
        let g = Graph::build(vec![
            bead("e", "open"),
            blocked_on(under("c1", "open", "e"), "up"),
            under("c2", "closed", "e"),
            bead("up", "open"),
        ]);
        let e = g.get("e").unwrap();
        assert_eq!(g.derived(e), Derived::Blocked);
        assert_eq!(g.blocked_by(e), Some(BlockedBy::NoReadyChild));
    }

    #[test]
    fn one_ready_descendant_makes_every_ancestor_ready() {
        let g = Graph::build(vec![
            bead("e", "open"),
            under("sub", "open", "e"),
            under("leaf", "open", "sub"),
            blocked_on(under("stuck", "open", "e"), "up"),
            bead("up", "open"),
        ]);
        assert_eq!(g.derived(g.get("sub").unwrap()), Derived::Ready);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::Ready);
    }

    #[test]
    fn parent_with_only_in_progress_work_left_reads_in_progress() {
        let g = Graph::build(vec![
            bead("e", "open"),
            under("busy", "in_progress", "e"),
            blocked_on(under("stuck", "open", "e"), "up"),
            bead("up", "open"),
        ]);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::InProgress);
    }

    #[test]
    fn children_inherit_an_ancestors_open_blockers() {
        let g = Graph::build(vec![
            blocked_on(bead("e", "open"), "up"),
            under("sub", "open", "e"),
            under("leaf", "open", "sub"),
            bead("up", "open"),
        ]);
        let leaf = g.get("leaf").unwrap();
        assert_eq!(g.derived(leaf), Derived::Blocked);
        assert_eq!(
            g.blocked_by(leaf),
            Some(BlockedBy::Ancestor(g.get("e").unwrap()))
        );

        let g = Graph::build(vec![
            blocked_on(bead("e", "open"), "up"),
            under("leaf", "open", "e"),
            bead("up", "closed"),
        ]);
        assert_eq!(g.derived(g.get("leaf").unwrap()), Derived::Ready);
        assert_eq!(g.derived(g.get("e").unwrap()), Derived::Ready);
    }

    #[test]
    fn parent_resolves_from_field_and_from_parent_child_dependency() {
        let mut via_field = bead("c1", "open");
        via_field.parent = Some("p".into());
        let mut via_dep = bead("c2", "open");
        via_dep.dependencies.push(dep("c2", "p", "parent-child"));
        let g = Graph::build(vec![bead("p", "open"), via_field, via_dep]);
        let p = g.get("p").unwrap();
        assert_eq!(g.parent[g.get("c1").unwrap()], Some(p));
        assert_eq!(g.parent[g.get("c2").unwrap()], Some(p));
        assert_eq!(g.children[p].len(), 2);
        assert_eq!(g.roots, vec![p]);
    }

    #[test]
    fn dangling_ids_are_ignored() {
        let mut a = bead("a", "open");
        a.parent = Some("ghost".into());
        a.dependencies.push(dep("a", "nobody", "blocks"));
        let g = Graph::build(vec![a]);
        assert_eq!(g.parent[0], None);
        assert!(g.blockers[0].is_empty());
        assert_eq!(g.derived(0), Derived::Ready);
        assert_eq!(g.roots, vec![0]);
    }

    #[test]
    fn parent_cycle_keeps_every_member_reachable() {
        let mut a = bead("a", "open");
        a.parent = Some("b".into());
        let mut b = bead("b", "open");
        b.parent = Some("a".into());
        let g = Graph::build(vec![a, b]);
        assert_eq!(g.roots.len(), 1);
        assert_eq!(g.descendants(g.roots[0]).len(), 1);
    }

    #[test]
    fn deserializes_absent_and_null_fields() {
        let b: Bead = serde_json::from_str(
            r#"{"id":"x","status":"weird","labels":null,"external_ref":"gh-12"}"#,
        )
        .unwrap();
        assert_eq!(b.status, Status::Other);
        assert!(b.labels.is_empty());
        assert_eq!(gh_number(&b), Some(12));
    }

    /// Priority decides among siblings even when status would have put the lower
    /// priority first: the in-progress P2 is older, yet the open P1 leads.
    #[test]
    fn higher_priority_sibling_leads_regardless_of_status() {
        let g = Graph::build(vec![
            bead("e", "open"),
            with_priority(under("p2", "in_progress", "e"), 2, "2026-01-01T00:00:00Z"),
            with_priority(under("p1", "open", "e"), 1, "2026-02-01T00:00:00Z"),
        ]);
        let kids: Vec<&str> = g.children[g.get("e").unwrap()]
            .iter()
            .map(|&i| g.beads[i].id.as_str())
            .collect();
        assert_eq!(kids, ["p1", "p2"]);
    }

    /// A P3 blocker is pulled above the P1 it blocks: dependency beats priority.
    #[test]
    fn blocker_precedes_the_bead_it_blocks() {
        let g = Graph::build(vec![
            bead("e", "open"),
            blocked_on(
                with_priority(under("hi", "open", "e"), 1, "2026-01-01T00:00:00Z"),
                "lo",
            ),
            with_priority(under("lo", "open", "e"), 3, "2026-02-01T00:00:00Z"),
        ]);
        let kids: Vec<&str> = g.children[g.get("e").unwrap()]
            .iter()
            .map(|&i| g.beads[i].id.as_str())
            .collect();
        assert_eq!(kids, ["lo", "hi"]);
    }

    /// A closed blocker no longer holds anything back, so priority order stands.
    #[test]
    fn closed_blocker_does_not_constrain_order() {
        let g = Graph::build(vec![
            bead("e", "open"),
            blocked_on(
                with_priority(under("hi", "open", "e"), 1, "2026-01-01T00:00:00Z"),
                "lo",
            ),
            with_priority(under("lo", "closed", "e"), 3, "2026-02-01T00:00:00Z"),
        ]);
        let kids: Vec<&str> = g.children[g.get("e").unwrap()]
            .iter()
            .map(|&i| g.beads[i].id.as_str())
            .collect();
        assert_eq!(kids, ["hi", "lo"]);
    }

    /// `a1` sits under root `A` and blocks `b1`, which sits under root `B`. The edge
    /// lifts to `A` -> `B`, so `A` comes first even though `B` holds the P1.
    #[test]
    fn lifted_blocker_orders_the_roots_of_its_subtrees() {
        let g = Graph::build(vec![
            with_priority(bead("A", "open"), 3, "2026-01-01T00:00:00Z"),
            with_priority(bead("B", "open"), 1, "2026-01-01T00:00:00Z"),
            under("a1", "open", "A"),
            blocked_on(under("b1", "open", "B"), "a1"),
        ]);
        let ids: Vec<&str> = g.roots.iter().map(|&i| g.beads[i].id.as_str()).collect();
        assert_eq!(ids, ["A", "B"]);
    }

    /// A blocks cycle among siblings cannot stall the walk: each sibling appears
    /// exactly once, and the smaller sort key goes first.
    #[test]
    fn blocks_cycle_among_siblings_yields_each_once() {
        let g = Graph::build(vec![
            bead("e", "open"),
            blocked_on(
                with_priority(under("x", "open", "e"), 1, "2026-01-01T00:00:00Z"),
                "y",
            ),
            blocked_on(
                with_priority(under("y", "open", "e"), 2, "2026-01-01T00:00:00Z"),
                "x",
            ),
        ]);
        let kids: Vec<&str> = g.children[g.get("e").unwrap()]
            .iter()
            .map(|&i| g.beads[i].id.as_str())
            .collect();
        assert_eq!(kids, ["x", "y"]);
    }

    /// Counts what would turn ○ the moment `x` closes: a direct dependent, the whole
    /// subtree of an epic it gates, and a parent whose only open child it blocks.
    /// A dependent still held by another blocker, or by a manual hold, is not.
    #[test]
    fn unblock_count_is_what_closing_the_bead_would_turn_ready() {
        let g = Graph::build(vec![
            bead("x", "open"),
            bead("y", "open"),
            blocked_on(bead("a", "open"), "x"),
            blocked_on(blocked_on(bead("b", "open"), "x"), "y"),
            blocked_on(bead("e", "open"), "x"),
            under("e1", "open", "e"),
            under("e2", "open", "e"),
            bead("p", "open"),
            blocked_on(under("p1", "open", "p"), "x"),
            blocked_on(bead("h", "blocked"), "x"),
        ]);
        assert_eq!(g.unblock_count(g.get("x").unwrap()), 6);
        assert_eq!(g.unblock_count(g.get("y").unwrap()), 0);
    }
}

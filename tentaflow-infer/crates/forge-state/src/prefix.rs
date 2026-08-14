// ===== File: prefix.rs — radix-tree prefix cache (SPEC §5.2) =====
// A page-granular radix tree keyed by the token-id sequence. Each node owns
// one physical KV page (page_size tokens) plus the tokens it stores; a chain
// of nodes root→…→n represents a cached prefix. A new request walks the
// tree matching its prompt tokens against cached prefixes and BORROWS the
// longest matching run of complete pages (refcounted, read-only): only the
// divergent suffix is prefilled. On completion the sequence donates its own
// freshly-prefilled complete pages back into the tree, extending the shared
// prefix for later requests.
//
// Correctness invariant: KV bytes are a deterministic function of the token
// prefix AND the kernel path that wrote them. A sequence donates every complete
// page it holds, prefilled AND decoded — the answer a conversation just heard is
// the prefix of the question it is about to ask, and refusing to cache it makes
// every turn recompute what the model itself said a moment ago. Decode writes
// K/V through a GEMV where prefill writes it through a tiled GEMM, so a borrowed
// decode page reproduces the sequence that donated it rather than a cold prefill
// of the same tokens; measured on a three-turn conversation the answers were
// identical either way. Sharing is at WHOLE-PAGE granularity, so a borrower never
// writes into a shared page (KV pages are append-only and its first write lands in
// a fresh page at the next page boundary); no copy-on-write is ever needed.
//
// A recurrent (DeltaNet/SSM) layer keeps no pages: everything the sequence has
// said is folded into one state matrix that the next token overwrites. Pages
// alone therefore describe only PART of a hybrid model's prefix, and reusing
// them without the matching state would resume mid-thought. So a node may also
// carry a STATE CHECKPOINT — an opaque slot id whose bytes the owner (the
// engine's state pool) holds — and a hybrid borrow is only ever taken at a node
// that has one. The checkpoint is what makes the pages beneath it meaningful,
// which is why the two are evicted with that dependency in mind: a leaf that
// lost its checkpoint has pages nothing can use, so it is the first page
// reclaimed rather than dead weight the tree keeps paying for.

use std::collections::HashMap;

/// Index into the node arena. Node 0 is always the (page-less) root.
pub type NodeId = usize;

/// Opaque id of a recurrent-state checkpoint. The tree only tracks WHICH slot
/// belongs to a node; the bytes live in the pool that minted the id.
pub type StateSlot = usize;

/// The page-less tree root. Donating from here caches a brand-new prefix
/// (cache-miss sequence with no borrow).
pub const ROOT: NodeId = 0;

/// What a borrow handed out: the shared physical pages in prefix order, the
/// pinned node they end at, how many tokens they cover, and the recurrent-state
/// checkpoint that node carries (hybrid models only).
pub struct Borrow {
    pub pages: Vec<i32>,
    pub node: Option<NodeId>,
    pub tokens: usize,
    pub state: Option<StateSlot>,
}

/// What a donation could not take: pages a concurrent insert already covered,
/// and checkpoints whose node already had one.
pub struct Donation {
    pub dup_pages: Vec<i32>,
    /// Leading pages the tree now owns or freed — the caller drains them from
    /// the sequence instead of freeing them itself.
    pub consumed: usize,
    pub dup_states: Vec<StateSlot>,
}

/// What an eviction reclaimed, for the caller to push back onto its free lists.
#[derive(Default)]
pub struct Reclaimed {
    pub pages: Vec<i32>,
    pub states: Vec<StateSlot>,
}

struct Node {
    /// Physical KV page id storing this node's `page_size` tokens. `-1` on the
    /// root (which stores no tokens).
    page: i32,
    /// The `page_size` tokens this node covers. Empty on the root.
    tokens: Box<[u32]>,
    parent: NodeId,
    /// Children keyed by their page tokens. Two children can never collide:
    /// identical tokens ⇒ identical KV ⇒ the same node (dedup).
    children: HashMap<Box<[u32]>, NodeId>,
    /// Active sequences whose borrowed prefix ends exactly at this node. A node
    /// with a live borrow (or any descendant borrow, via its children) is never
    /// evicted.
    refcount: usize,
    /// Monotonic access stamp for LRU eviction.
    last_access: u64,
    /// Recurrent state as of this node's last token, when one was donated.
    state: Option<StateSlot>,
    /// Access stamp of the checkpoint alone. Pages and checkpoints are reclaimed
    /// under different pressure, so one stamp for both would let a cheap page
    /// lookup keep an expensive checkpoint alive.
    state_access: u64,
}

/// The radix prefix cache. Owns tree nodes and the physical pages they hold;
/// the pages live in the same `KvCache` free-page id space, handed back to the
/// caller's free stack on eviction.
pub struct PrefixCache {
    nodes: Vec<Option<Node>>,
    free_ids: Vec<NodeId>,
    page_size: usize,
    tick: u64,
    /// Total pages currently held by tree nodes (root excluded).
    pages_held: usize,
    /// Nodes currently carrying a checkpoint.
    states_held: usize,
}

impl PrefixCache {
    pub fn new(page_size: usize) -> Self {
        let root = Node {
            page: -1,
            tokens: Box::new([]),
            parent: ROOT,
            children: HashMap::new(),
            refcount: 0,
            last_access: 0,
            state: None,
            state_access: 0,
        };
        Self {
            nodes: vec![Some(root)],
            free_ids: Vec::new(),
            page_size,
            tick: 1,
            pages_held: 0,
            states_held: 0,
        }
    }

    fn node(&self, id: NodeId) -> &Node {
        self.nodes[id].as_ref().expect("live node id")
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id].as_mut().expect("live node id")
    }

    fn alloc_node(&mut self, page: i32, tokens: Box<[u32]>, parent: NodeId) -> NodeId {
        let node = Node {
            page,
            tokens,
            parent,
            children: HashMap::new(),
            refcount: 0,
            last_access: self.tick,
            state: None,
            state_access: 0,
        };
        self.pages_held += 1;
        match self.free_ids.pop() {
            Some(id) => {
                self.nodes[id] = Some(node);
                id
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        }
    }

    /// Walk the tree matching `tokens` page-by-page (whole pages only), bounded
    /// by `max_shared_tokens`. Returns the deepest matched node and the number
    /// of pages matched. With `require_state`, the answer backs off to the
    /// deepest matched node that carries a checkpoint — pages past it describe
    /// tokens whose recurrent contribution nothing has recorded.
    fn walk(
        &self,
        tokens: &[u32],
        max_shared_tokens: usize,
        require_state: bool,
    ) -> (NodeId, usize) {
        let ps = self.page_size;
        let max_pages = max_shared_tokens / ps;
        let mut cur = ROOT;
        let mut depth = 0usize;
        let mut best = (ROOT, 0usize);
        while depth < max_pages && (depth + 1) * ps <= tokens.len() {
            let chunk = &tokens[depth * ps..(depth + 1) * ps];
            match self.node(cur).children.get(chunk) {
                Some(&child) => {
                    cur = child;
                    depth += 1;
                    if self.node(cur).state.is_some() {
                        best = (cur, depth);
                    }
                }
                None => break,
            }
        }
        if require_state {
            best
        } else {
            (cur, depth)
        }
    }

    /// Read-only longest-prefix length (in tokens, a multiple of `page_size`)
    /// this cache can serve for `tokens`, capped at `max_shared_tokens`. Used
    /// by admission to project the reduced prefill demand without pinning.
    pub fn match_len(
        &self,
        tokens: &[u32],
        max_shared_tokens: usize,
        require_state: bool,
    ) -> usize {
        self.walk(tokens, max_shared_tokens, require_state).1 * self.page_size
    }

    /// Borrow the longest cached prefix of `tokens` (≤ `max_shared_tokens`).
    /// The deepest node is refcounted so it — and its ancestors — cannot be
    /// evicted while the borrow is live, which is also what keeps its
    /// checkpoint alive until the borrower has copied it.
    pub fn acquire(
        &mut self,
        tokens: &[u32],
        max_shared_tokens: usize,
        require_state: bool,
    ) -> Borrow {
        let (deepest, depth) = self.walk(tokens, max_shared_tokens, require_state);
        if depth == 0 {
            return Borrow {
                pages: Vec::new(),
                node: None,
                tokens: 0,
                state: None,
            };
        }
        // Collect the page ids along the matched path (root → deepest).
        let mut pages = vec![0i32; depth];
        let mut cur = deepest;
        for slot in (0..depth).rev() {
            pages[slot] = self.node(cur).page;
            cur = self.node(cur).parent;
        }
        self.node_mut(deepest).refcount += 1;
        self.touch_path(deepest);
        let state = self.node(deepest).state;
        if state.is_some() {
            let stamp = self.tick;
            self.node_mut(deepest).state_access = stamp;
        }
        Borrow {
            pages,
            node: Some(deepest),
            tokens: depth * self.page_size,
            state,
        }
    }

    /// Release a borrow taken by `acquire` (decrement the deepest node's
    /// refcount). The node's pages become eviction candidates once no borrow
    /// and no descendant borrow references them.
    pub fn release(&mut self, node: NodeId) {
        let n = self.node_mut(node);
        debug_assert!(n.refcount > 0, "release without matching acquire");
        n.refcount = n.refcount.saturating_sub(1);
    }

    /// Donate a completed sequence's freshly-prefilled complete pages into the
    /// tree, extending the shared prefix from its borrowed `node`. `shared_pages`
    /// is how many leading pages were borrowed (already in the tree); `n_full`
    /// is the number of complete prefill-built pages. `tokens`/`pages` are the
    /// sequence's token ids and physical page ids.
    ///
    /// `states` are the sequence's recurrent checkpoints as `(token position,
    /// slot)`, each landing on the node that ends at that position. More than
    /// one matters: a checkpoint is the only place a hybrid borrow can begin, so
    /// a single one at the end of the prompt would serve a repeat of that whole
    /// prompt and nothing else — not the far more common request that shares its
    /// system prompt and asks a different question.
    pub fn donate(
        &mut self,
        node: NodeId,
        shared_pages: usize,
        n_full: usize,
        tokens: &[u32],
        pages: &[i32],
        states: &[(usize, StateSlot)],
    ) -> Donation {
        let ps = self.page_size;
        let mut cur = node;
        let mut dup_pages = Vec::new();
        let mut placed = vec![false; states.len()];
        for p in shared_pages..n_full {
            let chunk = &tokens[p * ps..(p + 1) * ps];
            let page = pages[p];
            if let Some(&child) = self.node(cur).children.get(chunk) {
                // The tree already stores this page — ours is a duplicate.
                dup_pages.push(page);
                self.touch(child);
                cur = child;
            } else {
                let toks: Box<[u32]> = chunk.into();
                let id = self.alloc_node(page, toks.clone(), cur);
                self.node_mut(cur).children.insert(toks, id);
                cur = id;
            }
            let at = (p + 1) * ps;
            for (index, &(pos, slot)) in states.iter().enumerate() {
                if pos == at && self.node(cur).state.is_none() {
                    self.tick += 1;
                    let stamp = self.tick;
                    let n = self.node_mut(cur);
                    n.state = Some(slot);
                    n.state_access = stamp;
                    self.states_held += 1;
                    placed[index] = true;
                }
            }
        }
        Donation {
            dup_pages,
            consumed: n_full,
            dup_states: states
                .iter()
                .zip(&placed)
                .filter(|(_, placed)| !**placed)
                .map(|(&(_, slot), _)| slot)
                .collect(),
        }
    }

    fn touch(&mut self, id: NodeId) {
        self.tick += 1;
        self.node_mut(id).last_access = self.tick;
    }

    fn touch_path(&mut self, mut id: NodeId) {
        self.tick += 1;
        let stamp = self.tick;
        while id != ROOT {
            self.node_mut(id).last_access = stamp;
            id = self.node(id).parent;
        }
    }

    /// Whether `id`'s subtree (itself included) contains any live borrow; such a
    /// node's page is still referenced by an active sequence and must not be
    /// evicted.
    fn subtree_pinned(&self, id: NodeId) -> bool {
        let n = self.node(id);
        n.refcount > 0 || n.children.values().any(|&c| self.subtree_pinned(c))
    }

    /// Pages the cache could reclaim right now (every page not referenced by a
    /// live borrow). Admission adds this to the free-page count.
    pub fn evictable_pages(&self) -> usize {
        self.pages_held - self.pinned_pages(ROOT)
    }

    /// Checkpoints the tree currently holds, live borrows included.
    pub fn cached_states(&self) -> usize {
        self.states_held
    }

    fn pinned_pages(&self, id: NodeId) -> usize {
        let n = self.node(id);
        let mut count = usize::from(id != ROOT && self.subtree_pinned(id));
        // Once a node is unpinned its whole subtree is unpinned, so only recurse
        // while this node is pinned (an ancestor of some live borrow).
        if id == ROOT || n.refcount > 0 || n.children.values().any(|&c| self.subtree_pinned(c)) {
            for &c in n.children.values() {
                count += self.pinned_pages(c);
            }
        }
        count
    }

    /// Evict refcount-0 leaves in LRU order until `count` pages are reclaimed or
    /// nothing more is evictable. A leaf without a checkpoint goes first: for a
    /// hybrid model its pages are unreachable — no borrow can ever end there —
    /// so keeping them would trade live capacity for nothing.
    pub fn evict(&mut self, count: usize) -> Reclaimed {
        let mut out = Reclaimed::default();
        while out.pages.len() < count {
            let Some(id) = self.lru_evictable_leaf() else {
                break;
            };
            let node = self.nodes[id].take().expect("chosen live node");
            self.node_mut(node.parent).children.remove(&node.tokens);
            self.free_ids.push(id);
            self.pages_held -= 1;
            if let Some(slot) = node.state {
                self.states_held -= 1;
                out.states.push(slot);
            }
            out.pages.push(node.page);
        }
        out
    }

    /// Drop up to `count` checkpoints, coldest first, from any unborrowed node —
    /// a checkpoint sits in the middle of a chain as often as at its end, so
    /// unlike pages it has no leaf-to-root order to respect. The nodes and their
    /// pages stay; only the recurrent state is given back.
    pub fn evict_states(&mut self, count: usize) -> Vec<StateSlot> {
        let mut freed = Vec::new();
        while freed.len() < count {
            let Some(id) = self
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(id, slot)| slot.as_ref().map(|n| (id, n)))
                .filter(|(_, n)| n.refcount == 0 && n.state.is_some())
                .min_by_key(|(_, n)| n.state_access)
                .map(|(id, _)| id)
            else {
                break;
            };
            let slot = self.node_mut(id).state.take().expect("filtered on state");
            self.states_held -= 1;
            freed.push(slot);
        }
        freed
    }

    fn lru_evictable_leaf(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|n| (id, n)))
            .filter(|(id, n)| *id != ROOT && n.refcount == 0 && n.children.is_empty())
            .min_by_key(|(_, n)| (n.state.is_some(), n.last_access))
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // page_size 4 keeps the token math legible in the assertions.
    const PS: usize = 4;

    fn toks(v: &[u32]) -> Vec<u32> {
        v.to_vec()
    }

    #[test]
    fn empty_cache_matches_nothing() {
        let pc = PrefixCache::new(PS);
        assert_eq!(pc.match_len(&toks(&[1, 2, 3, 4, 5]), 100, false), 0);
    }

    #[test]
    fn donate_then_match_shares_whole_pages() {
        let mut pc = PrefixCache::new(PS);
        // Sequence A prefilled 12 tokens (3 full pages) into physical pages 10,11,12.
        let a_tokens: Vec<u32> = (0..12).collect();
        let a_pages = [10i32, 11, 12];
        // No borrow (fresh), donate all 3 pages.
        let d = pc.donate(ROOT, 0, 3, &a_tokens, &a_pages, &[]);
        assert!(d.dup_pages.is_empty());
        assert_eq!(d.consumed, 3);
        assert_eq!(pc.evictable_pages(), 3);

        // Sequence B shares the same 12-token prefix, capped to leave ≥1 token.
        let b_tokens: Vec<u32> = (0..20).collect();
        let b = pc.acquire(&b_tokens, b_tokens.len() - 1, false);
        assert_eq!(b.pages, vec![10, 11, 12]);
        assert_eq!(b.tokens, 12);
        assert!(b.node.is_some());
        // Borrow pins the path: nothing is evictable.
        assert_eq!(pc.evictable_pages(), 0);
        pc.release(b.node.unwrap());
        assert_eq!(pc.evictable_pages(), 3);
    }

    #[test]
    fn divergent_suffix_branches() {
        let mut pc = PrefixCache::new(PS);
        let shared: Vec<u32> = vec![1, 1, 1, 1];
        // A: [1,1,1,1, 2,2,2,2]
        let a: Vec<u32> = [shared.clone(), vec![2, 2, 2, 2]].concat();
        pc.donate(ROOT, 0, 2, &a, &[10, 11], &[]);
        // B shares page 0 with A, diverges on page 1: [1,1,1,1, 3,3,3,3]
        let b: Vec<u32> = [shared.clone(), vec![3, 3, 3, 3]].concat();
        let borrow = pc.acquire(&b, 8, false);
        assert_eq!(borrow.pages, vec![10]); // only the common first page
        assert_eq!(borrow.tokens, 4);
        let node = borrow.node.unwrap();
        pc.release(node);
        // B donates its divergent page 20 as a new branch under page-0 node.
        let d = pc.donate(node, 1, 2, &b, &[10, 20], &[]);
        assert!(d.dup_pages.is_empty());
        assert_eq!(d.consumed, 2);
        assert_eq!(pc.evictable_pages(), 3); // pages 10, 11, 20
    }

    #[test]
    fn duplicate_donation_is_freed_not_reinserted() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..8).collect();
        pc.donate(ROOT, 0, 2, &t, &[10, 11], &[]);
        // A second sequence prefilled the same prefix into different physical
        // pages 30,31 (cache miss race) and donates — both are duplicates.
        let d = pc.donate(ROOT, 0, 2, &t, &[30, 31], &[]);
        assert_eq!(d.dup_pages, vec![30, 31]);
        assert_eq!(d.consumed, 2);
        assert_eq!(pc.evictable_pages(), 2); // still only 10, 11 held
    }

    #[test]
    fn eviction_is_lru_and_respects_borrows() {
        let mut pc = PrefixCache::new(PS);
        let a: Vec<u32> = (0..4).collect();
        let b: Vec<u32> = (100..104).collect();
        pc.donate(ROOT, 0, 1, &a, &[10], &[]);
        pc.donate(ROOT, 0, 1, &b, &[11], &[]);
        // Touch A's branch so B is the LRU victim.
        let borrow = pc.acquire(&a, 4, false);
        pc.release(borrow.node.unwrap());
        assert_eq!(pc.evict(1).pages, vec![11]);
        assert_eq!(pc.evictable_pages(), 1);

        // Re-borrow A and confirm it cannot be evicted.
        let borrow = pc.acquire(&a, 4, false);
        assert!(pc.evict(1).pages.is_empty());
        pc.release(borrow.node.unwrap());
        assert_eq!(pc.evict(1).pages, vec![10]);
        assert_eq!(pc.evictable_pages(), 0);
    }

    #[test]
    fn a_hybrid_borrow_stops_at_the_last_checkpoint() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..12).collect();
        // Three pages donated, but the checkpoint belongs to page 2 (token 8).
        pc.donate(ROOT, 0, 2, &t, &[10, 11], &[(8, 7)]);
        let borrow = pc.acquire(&t, 8, false);
        let from = borrow.node.expect("two pages match");
        pc.release(from);
        pc.donate(from, 2, 3, &t, &[10, 11, 12], &[]);
        // Pages alone would match all three; a state-requiring borrow stops at 8.
        assert_eq!(pc.match_len(&t, 12, false), 12);
        assert_eq!(pc.match_len(&t, 12, true), 8);
        let borrow = pc.acquire(&t, 12, true);
        assert_eq!(borrow.pages, vec![10, 11]);
        assert_eq!(borrow.tokens, 8);
        assert_eq!(borrow.state, Some(7));
    }

    #[test]
    fn a_second_checkpoint_for_the_same_node_comes_back() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..8).collect();
        assert!(pc
            .donate(ROOT, 0, 2, &t, &[10, 11], &[(8, 3)])
            .dup_states
            .is_empty());
        assert_eq!(pc.cached_states(), 1);
        let again = pc.donate(ROOT, 0, 2, &t, &[10, 11], &[(8, 4)]);
        assert_eq!(again.dup_states, vec![4]);
        assert_eq!(pc.cached_states(), 1);
    }

    #[test]
    fn several_checkpoints_land_on_the_nodes_that_end_where_they_were_taken() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..16).collect();
        // Checkpointy z pozycji 4 i 12; pozycja 20 nie ma swojego węzła.
        let donation = pc.donate(
            ROOT,
            0,
            4,
            &t,
            &[10, 11, 12, 13],
            &[(4, 1), (12, 2), (20, 3)],
        );
        assert_eq!(donation.dup_states, vec![3]);
        assert_eq!(pc.cached_states(), 2);
        // Zapytanie, które rozjeżdża się po ósmym tokenie, i tak ma z czego
        // ruszyć: bierze checkpoint z pozycji 4.
        let sibling: Vec<u32> = (0..8).chain(90..99).collect();
        assert_eq!(pc.match_len(&sibling, sibling.len() - 1, true), 4);
        // Pełny prefiks sięga głębszego.
        assert_eq!(pc.match_len(&t, 16, true), 12);
    }

    #[test]
    fn a_prefix_with_no_pages_of_its_own_keeps_its_checkpoint() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..4).collect();
        // Donating from the root with nothing to insert cannot place a state:
        // the root spans no tokens, so the slot has to come back.
        assert_eq!(
            pc.donate(ROOT, 0, 0, &t, &[], &[(4, 5)]).dup_states,
            vec![5]
        );
        assert_eq!(pc.cached_states(), 0);
    }

    #[test]
    fn a_leaf_without_a_checkpoint_is_reclaimed_first() {
        let mut pc = PrefixCache::new(PS);
        let a: Vec<u32> = (0..4).collect();
        let b: Vec<u32> = (100..104).collect();
        // The checkpointed branch is the OLDER one, so plain LRU would take it.
        pc.donate(ROOT, 0, 1, &a, &[10], &[(4, 1)]);
        pc.donate(ROOT, 0, 1, &b, &[11], &[]);
        let freed = pc.evict(1);
        assert_eq!(freed.pages, vec![11]);
        assert!(freed.states.is_empty());
        // Only once the useless leaf is gone does the checkpointed one go, and
        // it hands its checkpoint back with its page.
        let freed = pc.evict(1);
        assert_eq!(freed.pages, vec![10]);
        assert_eq!(freed.states, vec![1]);
        assert_eq!(pc.cached_states(), 0);
    }

    #[test]
    fn checkpoints_are_reclaimed_coldest_first_and_borrows_are_spared() {
        let mut pc = PrefixCache::new(PS);
        let a: Vec<u32> = (0..4).collect();
        let b: Vec<u32> = (100..104).collect();
        pc.donate(ROOT, 0, 1, &a, &[10], &[(4, 1)]);
        pc.donate(ROOT, 0, 1, &b, &[11], &[(4, 2)]);
        // Reading A's checkpoint makes B's the colder one.
        let borrow = pc.acquire(&a, 4, true);
        assert_eq!(borrow.state, Some(1));
        assert_eq!(pc.evict_states(1), vec![2]);
        // A is still borrowed, so its checkpoint is not a candidate.
        assert!(pc.evict_states(1).is_empty());
        pc.release(borrow.node.unwrap());
        assert_eq!(pc.evict_states(1), vec![1]);
        assert_eq!(pc.cached_states(), 0);
        // The pages outlive the checkpoints; only the state was given back.
        assert_eq!(pc.evictable_pages(), 2);
    }
}

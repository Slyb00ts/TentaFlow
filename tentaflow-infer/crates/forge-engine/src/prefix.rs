// ===== File: prefix.rs — radix-tree KV prefix cache (SPEC §5.2) =====
// A page-granular radix tree keyed by the token-id sequence. Each node owns
// one physical KV page (page_size tokens) plus the tokens it stores; a chain
// of nodes root→…→n represents a cached KV prefix. A new request walks the
// tree matching its prompt tokens against cached prefixes and BORROWS the
// longest matching run of complete pages (refcounted, read-only): only the
// divergent suffix is prefilled. On completion the sequence donates its own
// freshly-prefilled complete pages back into the tree, extending the shared
// prefix for later requests.
//
// Correctness invariant: KV bytes are a deterministic function of the token
// prefix AND the (prefill) kernel path. Only PREFILL-built pages are cached,
// so a borrowed prefix is byte-identical to what a cache-miss request would
// have prefilled — the borrower produces the exact same tokens as without the
// cache. Sharing is at WHOLE-PAGE granularity, so a borrower never writes into
// a shared page (KV pages are append-only and its first write lands in a fresh
// page at the next page boundary); no copy-on-write of a partial boundary page
// is ever needed.

use std::collections::HashMap;

/// Index into the node arena. Node 0 is always the (page-less) root.
pub type NodeId = usize;

/// The page-less tree root. Donating from here caches a brand-new prefix
/// (cache-miss sequence with no borrow).
pub const ROOT: NodeId = 0;

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
        };
        Self {
            nodes: vec![Some(root)],
            free_ids: Vec::new(),
            page_size,
            tick: 1,
            pages_held: 0,
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
    /// of pages matched.
    fn walk(&self, tokens: &[u32], max_shared_tokens: usize) -> (NodeId, usize) {
        let ps = self.page_size;
        let max_pages = max_shared_tokens / ps;
        let mut cur = ROOT;
        let mut depth = 0usize;
        while depth < max_pages && (depth + 1) * ps <= tokens.len() {
            let chunk = &tokens[depth * ps..(depth + 1) * ps];
            match self.node(cur).children.get(chunk) {
                Some(&child) => {
                    cur = child;
                    depth += 1;
                }
                None => break,
            }
        }
        (cur, depth)
    }

    /// Read-only longest-prefix length (in tokens, a multiple of `page_size`)
    /// this cache can serve for `tokens`, capped at `max_shared_tokens`. Used
    /// by admission to project the reduced prefill demand without pinning.
    pub fn match_len(&self, tokens: &[u32], max_shared_tokens: usize) -> usize {
        self.walk(tokens, max_shared_tokens).1 * self.page_size
    }

    /// Borrow the longest cached prefix of `tokens` (≤ `max_shared_tokens`).
    /// Returns the shared physical pages (in prefix order), the pinned deepest
    /// node, and the shared token count. The deepest node is refcounted so it —
    /// and its ancestors — cannot be evicted while the borrow is live.
    pub fn acquire(
        &mut self,
        tokens: &[u32],
        max_shared_tokens: usize,
    ) -> (Vec<i32>, Option<NodeId>, usize) {
        let (deepest, depth) = self.walk(tokens, max_shared_tokens);
        if depth == 0 {
            return (Vec::new(), None, 0);
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
        (pages, Some(deepest), depth * self.page_size)
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
    /// is the number of complete prefill-built pages (`prefilled_len/page_size`).
    /// `tokens`/`pages` are the sequence's token ids and physical page ids.
    ///
    /// Returns the duplicate page ids to return to the free stack (a
    /// concurrently-inserted continuation already covered them) and the count of
    /// leading pages the tree now owns or freed (so the caller drains them from
    /// the sequence instead of freeing them itself).
    pub fn donate(
        &mut self,
        node: NodeId,
        shared_pages: usize,
        n_full: usize,
        tokens: &[u32],
        pages: &[i32],
    ) -> (Vec<i32>, usize) {
        let ps = self.page_size;
        let mut cur = node;
        let mut dups = Vec::new();
        for p in shared_pages..n_full {
            let chunk = &tokens[p * ps..(p + 1) * ps];
            let page = pages[p];
            if let Some(&child) = self.node(cur).children.get(chunk) {
                // The tree already stores this page — ours is a duplicate.
                dups.push(page);
                self.touch(child);
                cur = child;
            } else {
                let toks: Box<[u32]> = chunk.into();
                let id = self.alloc_node(page, toks.clone(), cur);
                self.node_mut(cur).children.insert(toks, id);
                cur = id;
            }
        }
        (dups, n_full)
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
    /// nothing more is evictable, returning the freed physical page ids for the
    /// caller to push onto the KV free stack.
    pub fn evict(&mut self, count: usize) -> Vec<i32> {
        let mut freed = Vec::new();
        while freed.len() < count {
            let Some(id) = self.lru_evictable_leaf() else {
                break;
            };
            let node = self.nodes[id].take().expect("chosen live node");
            self.node_mut(node.parent).children.remove(&node.tokens);
            self.free_ids.push(id);
            self.pages_held -= 1;
            freed.push(node.page);
        }
        freed
    }

    fn lru_evictable_leaf(&self) -> Option<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|n| (id, n)))
            .filter(|(id, n)| *id != ROOT && n.refcount == 0 && n.children.is_empty())
            .min_by_key(|(_, n)| n.last_access)
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
        assert_eq!(pc.match_len(&toks(&[1, 2, 3, 4, 5]), 100), 0);
    }

    #[test]
    fn donate_then_match_shares_whole_pages() {
        let mut pc = PrefixCache::new(PS);
        // Sequence A prefilled 12 tokens (3 full pages) into physical pages 10,11,12.
        let a_tokens: Vec<u32> = (0..12).collect();
        let a_pages = [10i32, 11, 12];
        // No borrow (fresh), donate all 3 pages.
        let (dups, consumed) = pc.donate(ROOT, 0, 3, &a_tokens, &a_pages);
        assert!(dups.is_empty());
        assert_eq!(consumed, 3);
        assert_eq!(pc.evictable_pages(), 3);

        // Sequence B shares the same 12-token prefix, capped to leave ≥1 token.
        let b_tokens: Vec<u32> = (0..20).collect();
        let (pages, node, shared) = pc.acquire(&b_tokens, b_tokens.len() - 1);
        assert_eq!(pages, vec![10, 11, 12]);
        assert_eq!(shared, 12);
        assert!(node.is_some());
        // Borrow pins the path: nothing is evictable.
        assert_eq!(pc.evictable_pages(), 0);
        pc.release(node.unwrap());
        assert_eq!(pc.evictable_pages(), 3);
    }

    #[test]
    fn divergent_suffix_branches() {
        let mut pc = PrefixCache::new(PS);
        let shared: Vec<u32> = vec![1, 1, 1, 1];
        // A: [1,1,1,1, 2,2,2,2]
        let a: Vec<u32> = [shared.clone(), vec![2, 2, 2, 2]].concat();
        pc.donate(ROOT, 0, 2, &a, &[10, 11]);
        // B shares page 0 with A, diverges on page 1: [1,1,1,1, 3,3,3,3]
        let b: Vec<u32> = [shared.clone(), vec![3, 3, 3, 3]].concat();
        let (pages, node, shared_tok) = pc.acquire(&b, 8);
        assert_eq!(pages, vec![10]); // only the common first page
        assert_eq!(shared_tok, 4);
        pc.release(node.unwrap());
        // B donates its divergent page 20 as a new branch under page-0 node.
        let (dups, consumed) = pc.donate(node.unwrap(), 1, 2, &b, &[10, 20]);
        assert!(dups.is_empty());
        assert_eq!(consumed, 2);
        assert_eq!(pc.evictable_pages(), 3); // pages 10, 11, 20
    }

    #[test]
    fn duplicate_donation_is_freed_not_reinserted() {
        let mut pc = PrefixCache::new(PS);
        let t: Vec<u32> = (0..8).collect();
        pc.donate(ROOT, 0, 2, &t, &[10, 11]);
        // A second sequence prefilled the same prefix into different physical
        // pages 30,31 (cache miss race) and donates — both are duplicates.
        let (dups, consumed) = pc.donate(ROOT, 0, 2, &t, &[30, 31]);
        assert_eq!(dups, vec![30, 31]);
        assert_eq!(consumed, 2);
        assert_eq!(pc.evictable_pages(), 2); // still only 10, 11 held
    }

    #[test]
    fn eviction_is_lru_and_respects_borrows() {
        let mut pc = PrefixCache::new(PS);
        let a: Vec<u32> = (0..4).collect();
        let b: Vec<u32> = (100..104).collect();
        pc.donate(ROOT, 0, 1, &a, &[10]);
        pc.donate(ROOT, 0, 1, &b, &[11]);
        // Touch A's branch so B is the LRU victim.
        let (_p, node, _s) = pc.acquire(&a, 4);
        pc.release(node.unwrap());
        let freed = pc.evict(1);
        assert_eq!(freed, vec![11]);
        assert_eq!(pc.evictable_pages(), 1);

        // Re-borrow A and confirm it cannot be evicted.
        let (_p, node, _s) = pc.acquire(&a, 4);
        assert_eq!(pc.evict(1), Vec::<i32>::new());
        pc.release(node.unwrap());
        assert_eq!(pc.evict(1), vec![10]);
        assert_eq!(pc.evictable_pages(), 0);
    }
}

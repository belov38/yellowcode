use crate::boost_hash::mix64;

pub trait BoostKey: Eq {
    fn boost_hash(&self) -> i32;
}

const NIL: u32 = u32::MAX;
const MIN_BUCKETS: usize = 16;

struct Node<K, V> {
    entry: Option<(K, V)>,
    hash: i32,
    bucket: u32,
    prev: u32,
    next: u32,
}

pub struct BoostMap<K, V> {
    nodes: Vec<Node<K, V>>,
    free: Vec<u32>,
    head: u32,
    first: Vec<u32>,
    len: usize,
}

pub struct PreparedInsert<'a, K: BoostKey, V>(PreparedKind<'a, K, V>);

enum PreparedKind<'a, K: BoostKey, V> {
    Existing(&'a mut V),
    New {
        map: &'a mut BoostMap<K, V>,
        rehash: Option<RehashBuffers>,
    },
}

struct RehashBuffers {
    chain_head: Vec<u32>,
    chain_next: Vec<u32>,
    order: Vec<u32>,
    first: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InsertPlan {
    existing: Option<u32>,
    len: usize,
    nodes_len: usize,
    buckets: usize,
    rehash_count: Option<usize>,
    needs_node: bool,
}

pub struct InsertScratch {
    plan: InsertPlan,
    buffers: Option<RehashBuffers>,
    stage: usize,
}

impl InsertScratch {
    pub fn new(plan: InsertPlan) -> Self {
        let buffers = plan.rehash_count.map(|_| RehashBuffers {
            chain_head: Vec::new(),
            chain_next: Vec::new(),
            order: Vec::new(),
            first: Vec::new(),
        });
        Self {
            plan,
            buffers,
            stage: 0,
        }
    }
    pub fn stage_count(&self) -> usize {
        if self.buffers.is_some() { 4 } else { 0 }
    }
    pub fn stage_bound(&self, plan: InsertPlan) -> Option<u64> {
        if self.plan != plan {
            return None;
        }
        let count = plan.rehash_count?;
        let n = match self.stage {
            0 | 3 => count,
            1 | 2 => plan.nodes_len,
            _ => return None,
        };
        u64::try_from(n.checked_mul(2)?.checked_mul(std::mem::size_of::<u32>())?).ok()
    }
    pub fn backing_bytes(&self) -> Option<u64> {
        let Some(r) = &self.buffers else {
            return Some(0);
        };
        let n = r
            .chain_head
            .capacity()
            .checked_add(r.chain_next.capacity())?
            .checked_add(r.order.capacity())?
            .checked_add(r.first.capacity())?;
        u64::try_from(n.checked_mul(std::mem::size_of::<u32>())?).ok()
    }
    pub fn allocate_next(&mut self, plan: InsertPlan) -> Option<()> {
        if self.plan != plan {
            return None;
        }
        let r = self.buffers.as_mut()?;
        let count = plan.rehash_count?;
        let (v, n, fill) = match self.stage {
            0 => (&mut r.chain_head, count, true),
            1 => (&mut r.chain_next, plan.nodes_len, true),
            2 => (&mut r.order, plan.nodes_len, false),
            3 => (&mut r.first, count, true),
            _ => return None,
        };
        v.try_reserve_exact(n).ok()?;
        if fill {
            v.resize(n, NIL);
        }
        self.stage += 1;
        Some(())
    }
}

fn bucket_of(hash: i32, count: usize) -> u32 {
    (mix64(hash as i64 as u64) & (count as u64 - 1)) as u32
}

impl<K: BoostKey, V> PreparedInsert<'_, K, V> {
    pub fn commit(self, key: K, value: V, hash: i32) -> (Option<V>, Option<K>) {
        let (map, rehash) = match self.0 {
            PreparedKind::Existing(old) => return (Some(std::mem::replace(old, value)), Some(key)),
            PreparedKind::New { map, rehash } => (map, rehash),
        };
        if let Some(mut r) = rehash {
            let count = r.first.len();
            let mut n = map.head;
            while n != NIL {
                let node = &mut map.nodes[n as usize];
                let b = bucket_of(node.hash, count);
                node.bucket = b;
                if r.chain_head[b as usize] == NIL {
                    r.order.push(b);
                }
                r.chain_next[n as usize] = r.chain_head[b as usize];
                r.chain_head[b as usize] = n;
                n = node.next;
            }
            let mut last = NIL;
            map.head = NIL;
            for b in r.order {
                r.first[b as usize] = r.chain_head[b as usize];
                let mut n = r.chain_head[b as usize];
                while n != NIL {
                    map.nodes[n as usize].prev = last;
                    if last == NIL {
                        map.head = n;
                    } else {
                        map.nodes[last as usize].next = n;
                    }
                    last = n;
                    n = r.chain_next[n as usize];
                }
            }
            if last != NIL {
                map.nodes[last as usize].next = NIL;
            }
            map.first = r.first;
        }
        let b = bucket_of(hash, map.first.len());
        let node = Node {
            entry: Some((key, value)),
            hash,
            bucket: b,
            prev: NIL,
            next: NIL,
        };
        let n = if let Some(i) = map.free.pop() {
            map.nodes[i as usize] = node;
            i
        } else {
            let i = map.nodes.len() as u32;
            map.nodes.push(node);
            i
        };
        let first = map.first[b as usize];
        if first == NIL {
            let old = map.head;
            map.nodes[n as usize].next = old;
            if old != NIL {
                map.nodes[old as usize].prev = n;
            }
            map.head = n;
        } else {
            let previous = map.nodes[first as usize].prev;
            map.nodes[n as usize].prev = previous;
            map.nodes[n as usize].next = first;
            map.nodes[first as usize].prev = n;
            if previous == NIL {
                map.head = n;
            } else {
                map.nodes[previous as usize].next = n;
            }
        }
        map.first[b as usize] = n;
        map.len += 1;
        (None, None)
    }
}

impl<K: BoostKey, V> Default for BoostMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: BoostKey, V> BoostMap<K, V> {
    pub fn new() -> Self {
        BoostMap {
            nodes: Vec::new(),
            free: Vec::new(),
            head: NIL,
            first: vec![NIL; MIN_BUCKETS],
            len: 0,
        }
    }

    pub fn try_new() -> Option<Self> {
        let mut first = Vec::new();
        first.try_reserve_exact(MIN_BUCKETS).ok()?;
        first.resize(MIN_BUCKETS, NIL);
        Some(Self {
            nodes: Vec::new(),
            free: Vec::new(),
            head: NIL,
            first,
            len: 0,
        })
    }

    pub fn empty_buffer_bytes() -> usize {
        MIN_BUCKETS * std::mem::size_of::<u32>()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bucket_count(&self) -> usize {
        self.first.len()
    }

    pub fn try_with_buckets(bucket_count: usize, entries: usize) -> Option<Self> {
        if bucket_count < MIN_BUCKETS || !bucket_count.is_power_of_two() || entries > bucket_count {
            return None;
        }
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(entries).ok()?;
        let mut first = Vec::new();
        first.try_reserve_exact(bucket_count).ok()?;
        first.resize(bucket_count, NIL);
        Some(Self {
            nodes,
            free: Vec::new(),
            head: NIL,
            first,
            len: 0,
        })
    }

    pub fn storage_slots(&self) -> usize {
        self.nodes.len().saturating_add(self.first.len())
    }

    pub fn backing_bytes(&self) -> u64 {
        (self.nodes.capacity() as u64)
            .saturating_mul(std::mem::size_of::<Node<K, V>>() as u64)
            .saturating_add((self.free.capacity() as u64).saturating_mul(4))
            .saturating_add((self.first.capacity() as u64).saturating_mul(4))
    }

    pub fn checked_backing_bytes(&self) -> Option<u64> {
        let nodes = self
            .nodes
            .capacity()
            .checked_mul(std::mem::size_of::<Node<K, V>>())?;
        let free = self
            .free
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())?;
        let first = self
            .first
            .capacity()
            .checked_mul(std::mem::size_of::<u32>())?;
        u64::try_from(nodes.checked_add(free)?.checked_add(first)?).ok()
    }

    fn find(&self, k: &K) -> Option<u32> {
        let b = bucket_of(k.boost_hash(), self.first.len());
        let mut n = self.first[b as usize];
        while n != NIL {
            let node = &self.nodes[n as usize];
            if node.bucket != b {
                return None;
            }
            if node.entry.as_ref().is_some_and(|(nk, _)| nk == k) {
                return Some(n);
            }
            n = node.next;
        }
        None
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        let n = self.find(k)?;
        self.nodes[n as usize].entry.as_ref().map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        let n = self.find(k)?;
        self.nodes[n as usize].entry.as_mut().map(|(_, v)| v)
    }

    pub fn prepared_lookup(&self, key: &K) -> Option<u32> {
        self.find(key)
    }

    pub fn plan_insert(&self, existing: Option<u32>) -> Option<InsertPlan> {
        if let Some(i) = existing {
            self.nodes.get(i as usize)?.entry.as_ref()?;
        }
        let needs_node = existing.is_none() && self.free.is_empty();
        if needs_node && u32::try_from(self.nodes.len()).ok()? == NIL {
            return None;
        }
        let next = self.len.checked_add(usize::from(existing.is_none()))?;
        let rehash_count = if existing.is_none() && next > self.first.len() {
            let need = next
                .max(self.len.checked_add(self.len / 2)?)
                .checked_add(1)?;
            Some(need.max(MIN_BUCKETS).checked_next_power_of_two()?)
        } else {
            None
        };
        if rehash_count.is_some_and(|count| count > u32::MAX as usize) {
            return None;
        }
        Some(InsertPlan {
            existing,
            len: self.len,
            nodes_len: self.nodes.len(),
            buckets: self.first.len(),
            rehash_count,
            needs_node,
        })
    }
    fn plan_matches(&self, plan: InsertPlan) -> bool {
        self.len == plan.len
            && self.nodes.len() == plan.nodes_len
            && self.first.len() == plan.buckets
            && (plan.existing.is_some() || plan.needs_node == self.free.is_empty())
    }
    pub fn node_reserve_bound(&self, plan: InsertPlan) -> Option<u64> {
        if !self.plan_matches(plan) {
            return None;
        }
        if !plan.needs_node {
            return Some(0);
        }
        let slots = self.nodes.len().checked_add(1)?.checked_mul(2)?;
        u64::try_from(slots.checked_mul(std::mem::size_of::<Node<K, V>>())?).ok()
    }
    pub fn reserve_prepared_node(&mut self, plan: InsertPlan) -> Option<()> {
        if !self.plan_matches(plan) {
            return None;
        }
        if plan.needs_node {
            self.nodes.try_reserve_exact(1).ok()?;
        }
        Some(())
    }
    pub fn finish_prepared(
        &mut self,
        plan: InsertPlan,
        scratch: InsertScratch,
    ) -> Option<PreparedInsert<'_, K, V>> {
        if !self.plan_matches(plan)
            || scratch.plan != plan
            || scratch.stage != scratch.stage_count()
        {
            return None;
        }
        if let Some(i) = plan.existing {
            let (_, value) = self.nodes.get_mut(i as usize)?.entry.as_mut()?;
            return Some(PreparedInsert(PreparedKind::Existing(value)));
        }
        if plan.needs_node && self.nodes.len() == self.nodes.capacity() {
            return None;
        }
        Some(PreparedInsert(PreparedKind::New {
            map: self,
            rehash: scratch.buffers,
        }))
    }

    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        if let Some(n) = self.find(&k) {
            let slot = self.nodes[n as usize].entry.as_mut()?;
            return Some(std::mem::replace(&mut slot.1, v));
        }
        if self.len + 1 > self.first.len() {
            let need = (self.len + 1).max(self.len + self.len / 2) + 1;
            let mut count = MIN_BUCKETS;
            while count < need {
                count *= 2;
            }
            self.rehash(count);
        }
        let hash = k.boost_hash();
        let b = bucket_of(hash, self.first.len());
        let n = self.alloc(Node {
            entry: Some((k, v)),
            hash,
            bucket: b,
            prev: NIL,
            next: NIL,
        });
        let f = self.first[b as usize];
        if f == NIL {
            let old = self.head;
            self.nodes[n as usize].next = old;
            if old != NIL {
                self.nodes[old as usize].prev = n;
            }
            self.head = n;
        } else {
            let p = self.nodes[f as usize].prev;
            self.nodes[n as usize].prev = p;
            self.nodes[n as usize].next = f;
            self.nodes[f as usize].prev = n;
            if p == NIL {
                self.head = n;
            } else {
                self.nodes[p as usize].next = n;
            }
        }
        self.first[b as usize] = n;
        self.len += 1;
        None
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        let n = self.find(k)?;
        let (prev, next, b) = {
            let node = &self.nodes[n as usize];
            (node.prev, node.next, node.bucket)
        };
        if self.first[b as usize] == n {
            self.first[b as usize] = if next != NIL && self.nodes[next as usize].bucket == b {
                next
            } else {
                NIL
            };
        }
        if prev == NIL {
            self.head = next;
        } else {
            self.nodes[prev as usize].next = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        }
        let (_, v) = self.nodes[n as usize].entry.take()?;
        self.free.push(n);
        self.len -= 1;
        Some(v)
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.free.clear();
        self.first.iter_mut().for_each(|f| *f = NIL);
        self.head = NIL;
        self.len = 0;
    }

    pub fn drain(&mut self) -> impl Iterator<Item = (K, V)> + '_ {
        self.head = NIL;
        self.len = 0;
        self.first.fill(NIL);
        self.free.clear();
        self.nodes.drain(..).filter_map(|node| node.entry)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> + '_ {
        let mut n = self.head;
        std::iter::from_fn(move || {
            if n == NIL {
                return None;
            }
            let node = &self.nodes[n as usize];
            n = node.next;
            node.entry.as_ref().map(|(k, v)| (k, v))
        })
    }

    #[allow(clippy::expect_used, reason = "documented limit: node ids are u32")]
    fn alloc(&mut self, node: Node<K, V>) -> u32 {
        if let Some(i) = self.free.pop() {
            self.nodes[i as usize] = node;
            return i;
        }
        let i = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&i| i != NIL)
            .expect("BoostMap: more than u32::MAX - 1 nodes");
        self.nodes.push(node);
        i
    }

    fn rehash(&mut self, count: usize) {
        let mut chain_head = vec![NIL; count];
        let mut chain_next = vec![NIL; self.nodes.len()];
        let mut order: Vec<u32> = Vec::new();
        let mut n = self.head;
        while n != NIL {
            let node = &mut self.nodes[n as usize];
            let b = bucket_of(node.hash, count);
            node.bucket = b;
            if chain_head[b as usize] == NIL {
                order.push(b);
            }
            chain_next[n as usize] = chain_head[b as usize];
            chain_head[b as usize] = n;
            n = node.next;
        }
        self.first = vec![NIL; count];
        let mut last = NIL;
        self.head = NIL;
        for b in order {
            self.first[b as usize] = chain_head[b as usize];
            let mut n = chain_head[b as usize];
            while n != NIL {
                self.nodes[n as usize].prev = last;
                if last == NIL {
                    self.head = n;
                } else {
                    self.nodes[last as usize].next = n;
                }
                last = n;
                n = chain_next[n as usize];
            }
        }
        if last != NIL {
            self.nodes[last as usize].next = NIL;
        }
    }
}

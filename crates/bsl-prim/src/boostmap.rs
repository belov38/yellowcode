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

fn bucket_of(hash: i32, count: usize) -> u32 {
    (mix64(hash as i64 as u64) & (count as u64 - 1)) as u32
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

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bucket_count(&self) -> usize {
        self.first.len()
    }

    pub fn storage_slots(&self) -> usize {
        self.nodes.len().saturating_add(self.first.len())
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

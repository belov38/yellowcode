const ISORT_MAX: usize = 32;

pub fn msvc_sort<T>(v: &mut [T], less: &mut impl FnMut(&T, &T) -> bool) {
    sort_with_ideal(v, v.len(), less);
}

pub fn msvc_sort_with_scratch<T>(
    v: &mut [T],
    permutation: &mut [usize],
    less: &mut impl FnMut(&T, &T) -> bool,
) -> bool {
    if permutation.len() != v.len() {
        return false;
    }
    let n = v.len();
    for (i, slot) in permutation.iter_mut().enumerate() {
        *slot = i;
    }
    if n < 2 {
        return true;
    }
    {
        let items: &[T] = v;
        let mut lt = |x: usize, y: usize| match (items.get(x), items.get(y)) {
            (Some(a), Some(b)) => less(a, b),
            _ => false,
        };
        sort_range(permutation, 0, n, n, &mut lt);
    }
    apply_permutation(v, permutation);
    true
}

pub(crate) fn sort_with_ideal<T>(v: &mut [T], ideal: usize, less: &mut impl FnMut(&T, &T) -> bool) {
    let n = v.len();
    if n < 2 {
        return;
    }
    let mut p: Vec<usize> = (0..n).collect();
    {
        let items: &[T] = v;
        let mut lt = |x: usize, y: usize| match (items.get(x), items.get(y)) {
            (Some(a), Some(b)) => less(a, b),
            _ => false,
        };
        sort_range(&mut p, 0, n, ideal, &mut lt);
    }
    apply_permutation(v, &mut p);
}

fn apply_permutation<T>(v: &mut [T], p: &mut [usize]) {
    for i in 0..p.len() {
        let mut cur = i;
        while p[cur] != cur {
            let next = p[cur];
            if next == i {
                p[cur] = cur;
                break;
            }
            v.swap(cur, next);
            p[cur] = cur;
            cur = next;
        }
    }
}

type Lt<'a> = &'a mut dyn FnMut(usize, usize) -> bool;

fn sort_range(a: &mut [usize], mut first: usize, mut last: usize, mut ideal: usize, lt: Lt) {
    loop {
        if last - first <= ISORT_MAX {
            insertion(a, first, last, lt);
            return;
        }
        if ideal == 0 {
            make_heap(&mut a[first..last], lt);
            sort_heap(&mut a[first..last], lt);
            return;
        }
        let (mf, ms) = partition(a, first, last, lt);
        ideal = (ideal >> 1) + (ideal >> 2);
        if mf - first < last - ms {
            sort_range(a, first, mf, ideal, lt);
            first = ms;
        } else {
            sort_range(a, ms, last, ideal, lt);
            last = mf;
        }
    }
}

fn insertion(a: &mut [usize], first: usize, last: usize, lt: Lt) {
    let mut mid = first + 1;
    while mid < last {
        let val = a[mid];
        if lt(val, a[first]) {
            a.copy_within(first..mid, first + 1);
            a[first] = val;
        } else {
            let mut hole = mid;
            while hole > first && lt(val, a[hole - 1]) {
                a[hole] = a[hole - 1];
                hole -= 1;
            }
            a[hole] = val;
        }
        mid += 1;
    }
}

fn med3(a: &mut [usize], f: usize, m: usize, l: usize, lt: Lt) {
    if lt(a[m], a[f]) {
        a.swap(m, f);
    }
    if lt(a[l], a[m]) {
        a.swap(l, m);
        if lt(a[m], a[f]) {
            a.swap(m, f);
        }
    }
}

fn guess_median(a: &mut [usize], first: usize, mid: usize, last: usize, lt: Lt) {
    let count = last - first;
    if 40 < count {
        let step = (count + 1) >> 3;
        let two = step << 1;
        med3(a, first, first + step, first + two, lt);
        med3(a, mid - step, mid, mid + step, lt);
        med3(a, last - two, last - step, last, lt);
        med3(a, first + step, mid, last - step, lt);
    } else {
        med3(a, first, mid, last, lt);
    }
}

fn partition(a: &mut [usize], first: usize, last: usize, lt: Lt) -> (usize, usize) {
    let mid = first + ((last - first) >> 1);
    guess_median(a, first, mid, last - 1, lt);
    let mut pfirst = mid;
    let mut plast = pfirst + 1;
    while first < pfirst && !lt(a[pfirst - 1], a[pfirst]) && !lt(a[pfirst], a[pfirst - 1]) {
        pfirst -= 1;
    }
    while plast < last && !lt(a[plast], a[pfirst]) && !lt(a[pfirst], a[plast]) {
        plast += 1;
    }
    let mut gfirst = plast;
    let mut glast = pfirst;
    loop {
        while gfirst < last {
            if lt(a[pfirst], a[gfirst]) {
            } else if lt(a[gfirst], a[pfirst]) {
                break;
            } else if plast != gfirst {
                a.swap(plast, gfirst);
                plast += 1;
            } else {
                plast += 1;
            }
            gfirst += 1;
        }
        while first < glast {
            if lt(a[glast - 1], a[pfirst]) {
            } else if lt(a[pfirst], a[glast - 1]) {
                break;
            } else {
                pfirst -= 1;
                if pfirst != glast - 1 {
                    a.swap(pfirst, glast - 1);
                }
            }
            glast -= 1;
        }
        if glast == first && gfirst == last {
            return (pfirst, plast);
        }
        if glast == first {
            if plast != gfirst {
                a.swap(pfirst, plast);
            }
            plast += 1;
            a.swap(pfirst, gfirst);
            pfirst += 1;
            gfirst += 1;
        } else if gfirst == last {
            glast -= 1;
            pfirst -= 1;
            if glast != pfirst {
                a.swap(glast, pfirst);
            }
            plast -= 1;
            a.swap(pfirst, plast);
        } else {
            glast -= 1;
            a.swap(gfirst, glast);
            gfirst += 1;
        }
    }
}

fn push_heap_by_index(h: &mut [usize], mut hole: usize, top: usize, val: usize, lt: Lt) {
    while top < hole {
        let idx = (hole - 1) >> 1;
        if !lt(h[idx], val) {
            break;
        }
        h[hole] = h[idx];
        hole = idx;
    }
    h[hole] = val;
}

fn pop_heap_hole_by_index(h: &mut [usize], mut hole: usize, bottom: usize, val: usize, lt: Lt) {
    let top = hole;
    let mut idx = hole;
    let max_non_leaf = (bottom - 1) >> 1;
    while idx < max_non_leaf {
        idx = 2 * idx + 2;
        if lt(h[idx], h[idx - 1]) {
            idx -= 1;
        }
        h[hole] = h[idx];
        hole = idx;
    }
    if idx == max_non_leaf && bottom & 1 == 0 {
        h[hole] = h[bottom - 1];
        hole = bottom - 1;
    }
    push_heap_by_index(h, hole, top, val, lt);
}

fn make_heap(h: &mut [usize], lt: Lt) {
    let bottom = h.len();
    if bottom < 2 {
        return;
    }
    let mut hole = bottom >> 1;
    while hole > 0 {
        hole -= 1;
        let val = h[hole];
        pop_heap_hole_by_index(h, hole, bottom, val, lt);
    }
}

fn pop_heap(h: &mut [usize], lt: Lt) {
    let n = h.len();
    if n < 2 {
        return;
    }
    let val = h[n - 1];
    h[n - 1] = h[0];
    pop_heap_hole_by_index(h, 0, n - 1, val, lt);
}

fn sort_heap(h: &mut [usize], lt: Lt) {
    let mut n = h.len();
    while n >= 2 {
        pop_heap(&mut h[..n], lt);
        n -= 1;
    }
}

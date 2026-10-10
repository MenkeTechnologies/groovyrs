//! The `java.util` sequence classes a list handle can *be*: `LinkedList`,
//! `Vector`, `Stack`, `ArrayDeque` and `PriorityQueue`, plus the seeded
//! `java.util.Random` generator.
//!
//! A sequence is an ordinary list handle (so iteration, printing, `size`,
//! `contains`, `collect`, … are the list's own) tagged with its class in a side
//! table; the class decides `getClass()` and the methods the JDK type declares
//! on top of `List`. A `PriorityQueue` is stored in the JDK's binary-heap array
//! order, with the JDK's own sift routines, so what it prints and iterates in is
//! what Java prints.

use super::*;

/// Which JDK sequence class a list handle is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    LinkedList,
    Vector,
    Stack,
    ArrayDeque,
    PriorityQueue,
}

impl Kind {
    fn class(self) -> &'static str {
        match self {
            Kind::LinkedList => "java.util.LinkedList",
            Kind::Vector => "java.util.Vector",
            Kind::Stack => "java.util.Stack",
            Kind::ArrayDeque => "java.util.ArrayDeque",
            Kind::PriorityQueue => "java.util.PriorityQueue",
        }
    }

    fn is_deque(self) -> bool {
        matches!(self, Kind::LinkedList | Kind::ArrayDeque)
    }
}

#[derive(Clone)]
struct Tag {
    kind: Kind,
    /// A `PriorityQueue`'s comparator closure.
    cmp: Option<Value>,
}

thread_local! {
    static TAGS: RefCell<HashMap<u32, Tag>> = RefCell::new(HashMap::new());
}

/// Forget every tag (the heap was cleared).
pub(super) fn reset() {
    TAGS.with(|t| t.borrow_mut().clear());
    reset_flavors();
}

fn tag_of(v: &Value) -> Option<Tag> {
    let Value::Obj(id) = v else { return None };
    TAGS.with(|t| t.borrow().get(id).cloned())
}

/// The class name `getClass()` reports for a tagged list handle.
pub(super) fn class_of(v: &Value) -> Option<&'static str> {
    flavor_class(v).or_else(|| tag_of(v).map(|t| t.kind.class()))
}

/// Whether the tagged list `v` is a `Deque`/`Queue`/… named `short`.
pub(super) fn is_a(v: &Value, short: &str) -> Option<bool> {
    let tag = tag_of(v)?;
    Some(match short {
        "Deque" => tag.kind.is_deque() || tag.kind == Kind::ArrayDeque,
        "Queue" => tag.kind.is_deque() || tag.kind == Kind::PriorityQueue,
        "LinkedList" => tag.kind == Kind::LinkedList,
        "Vector" => matches!(tag.kind, Kind::Vector | Kind::Stack),
        "Stack" => tag.kind == Kind::Stack,
        "ArrayDeque" => tag.kind == Kind::ArrayDeque,
        "PriorityQueue" => tag.kind == Kind::PriorityQueue,
        "List" | "RandomAccess" | "AbstractList" | "AbstractSequentialList" => {
            matches!(tag.kind, Kind::LinkedList | Kind::Vector | Kind::Stack)
        }
        "ArrayList" => false,
        _ => return None,
    })
}

fn elements(v: &Value) -> Vec<Value> {
    as_list_raw(v).unwrap_or_default()
}

fn store(v: &Value, items: Vec<Value>) {
    if let Value::Obj(id) = v {
        list_store(*id, items, true);
    }
}

/// `new LinkedList(…)`, `new ArrayDeque(…)`, … — `None` for any other class.
pub(super) fn construct(vm: &mut VM, simple: &str, args: &[Value]) -> Option<Value> {
    let kind = match simple {
        "LinkedList" => Kind::LinkedList,
        "Vector" => Kind::Vector,
        "Stack" => Kind::Stack,
        "ArrayDeque" => Kind::ArrayDeque,
        "PriorityQueue" => Kind::PriorityQueue,
        _ => return None,
    };
    // The capacity argument (`new ArrayDeque(16)`) is only a hint.
    let mut seed: Vec<Value> = Vec::new();
    let mut cmp = None;
    for a in args {
        if closure_meta(a).is_some() {
            cmp = Some(a.clone());
        } else if as_i64(a).is_none() || matches!(a, Value::Obj(_)) {
            seed = iteration_elements(a);
        }
    }
    if kind == Kind::ArrayDeque && seed.iter().any(|v| matches!(v, Value::Undef)) {
        raise_opt(vm, "NullPointerException", None);
        return Some(Value::Undef);
    }
    let handle = glist(Vec::new());
    let Value::Obj(id) = handle else { return None };
    TAGS.with(|t| t.borrow_mut().insert(id, Tag { kind, cmp }));
    if kind == Kind::PriorityQueue {
        // `PriorityQueue(Collection)` heapifies the copied array bottom-up.
        if seed.iter().any(|v| matches!(v, Value::Undef)) {
            raise_opt(vm, "NullPointerException", None);
            return Some(Value::Undef);
        }
        let cmp = tag_of(&handle).and_then(|t| t.cmp);
        for i in (0..seed.len() / 2).rev() {
            let x = seed[i].clone();
            if let Err(e) = sift_down(vm, &cmp, &mut seed, i, x) {
                fault(vm, e);
                return Some(Value::Undef);
            }
        }
        store(&handle, seed);
    } else {
        store(&handle, seed);
    }
    Some(handle)
}

// ── PriorityQueue ───────────────────────────────────────────────────────────

/// The queue's comparator applied to two elements: the closure when it has one
/// (a one-parameter closure is a key extractor, as in the GDK), else natural
/// order.
fn pq_cmp(
    vm: &mut VM,
    cmp: &Option<Value>,
    a: &Value,
    b: &Value,
) -> Result<std::cmp::Ordering, String> {
    match cmp {
        Some(c) => OrderBy::of(std::slice::from_ref(c)).apply(vm, a, b),
        None => compare_values(vm, a, b),
    }
}

fn sift_up(
    vm: &mut VM,
    cmp: &Option<Value>,
    q: &mut [Value],
    mut k: usize,
    x: Value,
) -> Result<(), String> {
    while k > 0 {
        let parent = (k - 1) >> 1;
        if pq_cmp(vm, cmp, &x, &q[parent])?.is_ge() {
            break;
        }
        q[k] = q[parent].clone();
        k = parent;
    }
    q[k] = x;
    Ok(())
}

fn sift_down(
    vm: &mut VM,
    cmp: &Option<Value>,
    q: &mut [Value],
    mut k: usize,
    x: Value,
) -> Result<(), String> {
    let n = q.len();
    let half = n >> 1;
    while k < half {
        let mut child = 2 * k + 1;
        let right = child + 1;
        if right < n && pq_cmp(vm, cmp, &q[child], &q[right])?.is_gt() {
            child = right;
        }
        if pq_cmp(vm, cmp, &x, &q[child])?.is_le() {
            break;
        }
        q[k] = q[child].clone();
        k = child;
    }
    q[k] = x;
    Ok(())
}

fn pq_add(vm: &mut VM, recv: &Value, x: Value) -> Result<(), ()> {
    if matches!(x, Value::Undef) {
        raise_opt(vm, "NullPointerException", None);
        return Err(());
    }
    let cmp = tag_of(recv).and_then(|t| t.cmp);
    let mut q = elements(recv);
    q.push(x.clone());
    let k = q.len() - 1;
    // The JDK compares the first element against itself so a non-comparable
    // lone element raises at `add`; natural order here never raises.
    if let Err(e) = sift_up(vm, &cmp, &mut q, k, x) {
        fault(vm, e);
        return Err(());
    }
    store(recv, q);
    Ok(())
}

/// `PriorityQueue.removeAt(i)`.
fn pq_remove_at(vm: &mut VM, recv: &Value, i: usize) -> Result<(), String> {
    let cmp = tag_of(recv).and_then(|t| t.cmp);
    let mut q = elements(recv);
    let last = q.len() - 1;
    if i == last {
        q.pop();
    } else {
        let moved = q.pop().unwrap_or(Value::Undef);
        sift_down(vm, &cmp, &mut q, i, moved.clone())?;
        if identical(&q[i], &moved) {
            sift_up(vm, &cmp, &mut q, i, moved)?;
        }
    }
    store(recv, q);
    Ok(())
}

/// Reference identity for the "did `siftDown` leave `moved` in place" test.
fn identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Obj(x), Value::Obj(y)) => x == y,
        _ => java_equals(a, b),
    }
}

// ── Dispatch ────────────────────────────────────────────────────────────────

fn no_such_element(vm: &mut VM) -> Value {
    raise_opt(vm, "NoSuchElementException", None);
    Value::Undef
}

/// A method the JDK class declares beyond `List`. `None` leaves it to the list
/// dispatch (which is right for everything a `LinkedList` shares with
/// `ArrayList`).
pub(super) fn dispatch(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    let tag = tag_of(recv)?;
    // Not a `List`: the index-based members do not exist on a queue or an
    // `ArrayDeque`.
    if matches!(tag.kind, Kind::ArrayDeque | Kind::PriorityQueue)
        && matches!(
            method,
            "reverse"
                | "get"
                | "getAt"
                | "indexOf"
                | "lastIndexOf"
                | "set"
                | "subList"
                | "removeAt"
        )
    {
        return Some(raise_missing_method(vm, recv, method, args));
    }
    match tag.kind {
        Kind::PriorityQueue => pq_method(vm, recv, &tag, method, args),
        Kind::Stack => stack_method(vm, recv, method, args),
        Kind::Vector => None,
        Kind::LinkedList | Kind::ArrayDeque => deque_method(vm, recv, tag.kind, method, args),
    }
}

fn stack_method(vm: &mut VM, recv: &Value, method: &str, args: &[Value]) -> Option<Value> {
    let mut items = elements(recv);
    match (method, args.len()) {
        ("push", 1) => {
            items.push(args[0].clone());
            store(recv, items);
            Some(args[0].clone())
        }
        ("pop", 0) => match items.pop() {
            Some(v) => {
                store(recv, items);
                Some(v)
            }
            None => {
                raise_opt(vm, "EmptyStackException", None);
                Some(Value::Undef)
            }
        },
        ("peek", 0) => match items.last() {
            Some(v) => Some(v.clone()),
            None => {
                raise_opt(vm, "EmptyStackException", None);
                Some(Value::Undef)
            }
        },
        ("empty", 0) => Some(Value::bool(items.is_empty())),
        ("search", 1) => Some(Value::int(
            items
                .iter()
                .rposition(|v| java_equals(v, &args[0]))
                .map_or(-1, |i| (items.len() - i) as i64),
        )),
        ("firstElement", 0) | ("lastElement", 0) => match if method == "firstElement" {
            items.first()
        } else {
            items.last()
        } {
            Some(v) => Some(v.clone()),
            None => Some(no_such_element(vm)),
        },
        _ => None,
    }
}

fn deque_method(
    vm: &mut VM,
    recv: &Value,
    kind: Kind,
    method: &str,
    args: &[Value],
) -> Option<Value> {
    let arrays = kind == Kind::ArrayDeque;
    let mut items = elements(recv);
    // `ArrayDeque` refuses `null` elements outright.
    if arrays
        && matches!(
            method,
            "add" | "addFirst" | "addLast" | "offer" | "offerFirst" | "offerLast" | "push"
        )
        && matches!(args.first(), Some(Value::Undef))
    {
        raise_opt(vm, "NullPointerException", None);
        return Some(Value::Undef);
    }
    let done = |items: Vec<Value>, out: Value| {
        store(recv, items);
        Some(out)
    };
    match (method, args.len()) {
        ("addFirst", 1) | ("push", 1) => {
            items.insert(0, args[0].clone());
            done(items, Value::Undef)
        }
        ("offerFirst", 1) => {
            items.insert(0, args[0].clone());
            done(items, Value::bool(true))
        }
        ("addLast", 1) => {
            items.push(args[0].clone());
            done(items, Value::Undef)
        }
        ("offer", 1) | ("offerLast", 1) => {
            items.push(args[0].clone());
            done(items, Value::bool(true))
        }
        ("add", 1) if arrays => {
            items.push(args[0].clone());
            done(items, Value::bool(true))
        }
        ("pop", 0) | ("removeFirst", 0) | ("remove", 0) => {
            if items.is_empty() {
                return Some(no_such_element(vm));
            }
            let v = items.remove(0);
            done(items, v)
        }
        ("removeLast", 0) => match items.pop() {
            Some(v) => done(items, v),
            None => Some(no_such_element(vm)),
        },
        ("poll", 0) | ("pollFirst", 0) => {
            if items.is_empty() {
                return Some(Value::Undef);
            }
            let v = items.remove(0);
            done(items, v)
        }
        ("pollLast", 0) => match items.pop() {
            Some(v) => done(items, v),
            None => Some(Value::Undef),
        },
        ("peek", 0) | ("peekFirst", 0) => Some(items.first().cloned().unwrap_or(Value::Undef)),
        ("peekLast", 0) => Some(items.last().cloned().unwrap_or(Value::Undef)),
        ("element", 0) | ("getFirst", 0) => match items.first() {
            Some(v) => Some(v.clone()),
            None => Some(no_such_element(vm)),
        },
        ("getLast", 0) => match items.last() {
            Some(v) => Some(v.clone()),
            None => Some(no_such_element(vm)),
        },
        ("removeFirstOccurrence", 1) => {
            let hit = items.iter().position(|v| java_equals(v, &args[0]));
            if let Some(i) = hit {
                items.remove(i);
                store(recv, items);
            }
            Some(Value::bool(hit.is_some()))
        }
        ("removeLastOccurrence", 1) => {
            let hit = items.iter().rposition(|v| java_equals(v, &args[0]));
            if let Some(i) = hit {
                items.remove(i);
                store(recv, items);
            }
            Some(Value::bool(hit.is_some()))
        }
        // `ArrayDeque.remove(Object)`; a `LinkedList` keeps `List.remove(int)`.
        ("remove", 1) if arrays => {
            let hit = items.iter().position(|v| java_equals(v, &args[0]));
            if let Some(i) = hit {
                items.remove(i);
                store(recv, items);
            }
            Some(Value::bool(hit.is_some()))
        }
        ("descendingIterator", 0) => {
            items.reverse();
            Some(heap_push(HeapObj::Iter {
                class: "java.util.LinkedList$DescendingIterator",
                items,
                pos: 0,
            }))
        }
        _ => None,
    }
}

fn pq_method(vm: &mut VM, recv: &Value, tag: &Tag, method: &str, args: &[Value]) -> Option<Value> {
    match (method, args.len()) {
        ("add", 1) | ("offer", 1) | ("leftShift", 1) => {
            Some(match pq_add(vm, recv, args[0].clone()) {
                Ok(()) if method == "leftShift" => recv.clone(),
                Ok(()) => Value::bool(true),
                Err(()) => Value::Undef,
            })
        }
        ("addAll", 1) => {
            for v in iteration_elements(&args[0]) {
                if pq_add(vm, recv, v).is_err() {
                    return Some(Value::Undef);
                }
            }
            Some(Value::bool(true))
        }
        ("poll", 0) | ("remove", 0) => {
            let mut q = elements(recv);
            if q.is_empty() {
                return Some(if method == "poll" {
                    Value::Undef
                } else {
                    no_such_element(vm)
                });
            }
            let result = q[0].clone();
            let x = q.pop().unwrap_or(Value::Undef);
            if !q.is_empty() {
                if let Err(e) = sift_down(vm, &tag.cmp, &mut q, 0, x) {
                    fault(vm, e);
                    return Some(Value::Undef);
                }
            }
            store(recv, q);
            Some(result)
        }
        ("peek", 0) => Some(elements(recv).first().cloned().unwrap_or(Value::Undef)),
        ("element", 0) => match elements(recv).first() {
            Some(v) => Some(v.clone()),
            None => Some(no_such_element(vm)),
        },
        ("remove", 1) => {
            let q = elements(recv);
            match q.iter().position(|v| java_equals(v, &args[0])) {
                Some(i) => {
                    if let Err(e) = pq_remove_at(vm, recv, i) {
                        fault(vm, e);
                    }
                    Some(Value::bool(true))
                }
                None => Some(Value::bool(false)),
            }
        }
        ("comparator", 0) => Some(tag.cmp.clone().unwrap_or(Value::Undef)),
        _ => None,
    }
}

// ── java.util.Random ────────────────────────────────────────────────────────

const MULTIPLIER: u64 = 0x5_DEEC_E66D;
const MASK: u64 = (1 << 48) - 1;

/// The state of a `java.util.Random`: the 48-bit LCG seed and the cached second
/// Gaussian of the polar method.
#[derive(Clone)]
pub(super) struct RandomState {
    seed: u64,
    next_gaussian: Option<f64>,
}

impl RandomState {
    pub(super) fn new(seed: i64) -> Self {
        RandomState {
            seed: (seed as u64 ^ MULTIPLIER) & MASK,
            next_gaussian: None,
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULTIPLIER).wrapping_add(0xB) & MASK;
        (self.seed >> (48 - bits)) as i64 as i32
    }

    fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    fn next_int_bound(&mut self, bound: i32) -> i32 {
        let mut r = self.next(31);
        let m = bound - 1;
        if bound & m == 0 {
            return ((i64::from(bound) * i64::from(r)) >> 31) as i32;
        }
        let mut u = r;
        loop {
            r = u % bound;
            if u.wrapping_sub(r).wrapping_add(m) >= 0 {
                return r;
            }
            u = self.next(31);
        }
    }

    /// `RandomSupport.boundedNextInt(rng, origin, bound)`.
    fn next_int_range(&mut self, origin: i32, bound: i32) -> i32 {
        let mut r = self.next_int();
        if origin < bound {
            let n = bound.wrapping_sub(origin);
            let m = n.wrapping_sub(1);
            if n & m == 0 {
                r = (r & m).wrapping_add(origin);
            } else if n > 0 {
                let mut u = ((r as u32) >> 1) as i32;
                loop {
                    r = u % n;
                    if u.wrapping_add(m).wrapping_sub(r) >= 0 {
                        break;
                    }
                    u = ((self.next_int() as u32) >> 1) as i32;
                }
                r = r.wrapping_add(origin);
            } else {
                while r < origin || r >= bound {
                    r = self.next_int();
                }
            }
        }
        r
    }

    fn next_long(&mut self) -> i64 {
        (i64::from(self.next(32)) << 32).wrapping_add(i64::from(self.next(32)))
    }

    fn next_double(&mut self) -> f64 {
        let hi = (i64::from(self.next(26))) << 27;
        let lo = i64::from(self.next(27));
        (hi + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 / (1 << 24) as f32
    }

    fn next_boolean(&mut self) -> bool {
        self.next(1) != 0
    }

    fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.next_gaussian.take() {
            return g;
        }
        loop {
            let v1 = 2.0 * self.next_double() - 1.0;
            let v2 = 2.0 * self.next_double() - 1.0;
            let s = v1 * v1 + v2 * v2;
            if s < 1.0 && s != 0.0 {
                // `StrictMath.sqrt/log`.
                let multiplier = (-2.0 * s.ln() / s).sqrt();
                self.next_gaussian = Some(v2 * multiplier);
                return v1 * multiplier;
            }
        }
    }
}

/// An unseeded `new Random()`: JDK seeds from the clock mixed with a uniquifier.
pub(super) fn fresh_seed() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ 0x1234_5678_9ABC_DEF1) as i64
}

/// Call `method` on the `java.util.Random` heap object `id`.
pub(super) fn random_method(vm: &mut VM, id: u32, method: &str, args: &[Value]) -> Option<Value> {
    let mut st = HEAP.with(|h| match h.borrow().get(id as usize) {
        Some(HeapObj::Random(s)) => Some(s.clone()),
        _ => None,
    })?;
    let int_arg = |i: usize| args.get(i).and_then(as_i64);
    let out = match (method, args.len()) {
        ("nextInt", 0) => Value::int(i64::from(st.next_int())),
        ("nextInt", 1) => {
            let bound = int_arg(0).unwrap_or(0) as i32;
            if bound <= 0 {
                raise(vm, "IllegalArgumentException", "bound must be positive");
                return Some(Value::Undef);
            }
            Value::int(i64::from(st.next_int_bound(bound)))
        }
        ("nextInt", 2) => {
            let (origin, bound) = (
                int_arg(0).unwrap_or(0) as i32,
                int_arg(1).unwrap_or(0) as i32,
            );
            if origin >= bound {
                raise(
                    vm,
                    "IllegalArgumentException",
                    "bound must be greater than origin",
                );
                return Some(Value::Undef);
            }
            Value::int(i64::from(st.next_int_range(origin, bound)))
        }
        ("nextLong", 0) => Value::int(st.next_long()),
        ("nextDouble", 0) => Value::float(st.next_double()),
        ("nextFloat", 0) => float_value(st.next_float()),
        ("nextBoolean", 0) => Value::bool(st.next_boolean()),
        ("nextGaussian", 0) => Value::float(st.next_gaussian()),
        ("setSeed", 1) => {
            st = RandomState::new(int_arg(0).unwrap_or(0));
            Value::Undef
        }
        _ => return None,
    };
    HEAP.with(|h| {
        if let Some(HeapObj::Random(slot)) = h.borrow_mut().get_mut(id as usize) {
            *slot = st;
        }
    });
    Some(out)
}

// ── Wrapper flavors: unmodifiable, fixed-size, named JDK wrappers ───────────

/// What a flavored collection refuses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Writes {
    /// Any write: `Collections.unmodifiableList`, `List.of`, `asImmutable()`.
    Frozen,
    /// Replacing an element is fine, resizing is not: `Arrays.asList`.
    FixedSize,
    /// Everything is allowed; only the class name differs.
    Open,
}

#[derive(Clone, Copy)]
struct Flavor {
    class: &'static str,
    writes: Writes,
}

thread_local! {
    static FLAVORS: RefCell<HashMap<u32, Flavor>> = RefCell::new(HashMap::new());
    /// Set once any flavor exists, so the write guards cost one flag read on
    /// the hot `list[i] = v` / `m[k] = v` paths of a script that never wraps.
    static ANY_FLAVOR: Cell<bool> = const { Cell::new(false) };
}

/// Tag the collection handle `v` with a JDK wrapper class and write policy.
pub(super) fn set_flavor(v: &Value, class: &'static str, writes: Writes) {
    if let Value::Obj(id) = v {
        FLAVORS.with(|f| f.borrow_mut().insert(*id, Flavor { class, writes }));
        ANY_FLAVOR.with(|a| a.set(true));
    }
}

/// The wrapper class of a flavored handle.
pub(super) fn flavor_class(v: &Value) -> Option<&'static str> {
    let Value::Obj(id) = v else { return None };
    if !ANY_FLAVOR.with(Cell::get) {
        return None;
    }
    FLAVORS.with(|f| f.borrow().get(id).map(|fl| fl.class))
}

/// Whether a write to the collection `id` must be refused. `resizes` says the
/// write changes the element count. A refused write raises
/// `UnsupportedOperationException` here, so the caller only has to stop.
pub(super) fn write_refused(id: u32, resizes: bool) -> bool {
    if !ANY_FLAVOR.with(Cell::get) {
        return false;
    }
    let refused = FLAVORS.with(|f| match f.borrow().get(&id) {
        Some(fl) => match fl.writes {
            Writes::Frozen => true,
            Writes::FixedSize => resizes,
            Writes::Open => false,
        },
        None => false,
    });
    if refused {
        with_vm(|vm| raise_opt(vm, "UnsupportedOperationException", None));
    }
    refused
}

/// Drop every flavor (the heap was cleared).
pub(super) fn reset_flavors() {
    FLAVORS.with(|f| f.borrow_mut().clear());
    ANY_FLAVOR.with(|a| a.set(false));
}

/// A copy of the collection `v` (a list, set or map handle, or a range), as a
/// new handle flavored `class` / `writes`. `None` when `v` is not a collection.
fn wrapped_copy(v: &Value, class: &'static str, writes: Writes) -> Option<Value> {
    let out = if let Some((items, kind)) = as_set(v) {
        make_set(set_elements(&items, kind), kind)
    } else if is_omap(v) {
        let (entries, kind) = as_omap_kind(v)?;
        gmap_kind(entries, kind)
    } else if as_list_raw(v).is_some() || as_range(v).is_some() || matches!(v, Value::Array(_)) {
        glist(iteration_elements(v))
    } else {
        return None;
    };
    set_flavor(&out, class, writes);
    Some(out)
}

/// `asImmutable()` / `asUnmodifiable()`: an unmodifiable collection over the
/// receiver's elements. It is a copy, not a view, so a later write to the
/// original is not seen through it.
pub(super) fn unmodifiable_copy(v: &Value) -> Option<Value> {
    let class = if as_set(v).is_some() {
        "java.util.Collections$UnmodifiableSet"
    } else if is_omap(v) {
        "java.util.Collections$UnmodifiableMap"
    } else {
        "java.util.Collections$UnmodifiableRandomAccessList"
    };
    wrapped_copy(v, class, Writes::Frozen)
}

/// The `java.util` factory statics that answer a wrapper collection.
pub(super) fn static_call(vm: &mut VM, class: &str, method: &str, args: &[Value]) -> Option<Value> {
    let arg0 = args.first().cloned().unwrap_or(Value::Undef);
    let list_of = |items: Vec<Value>, class: &'static str, writes: Writes| {
        let out = glist(items);
        set_flavor(&out, class, writes);
        out
    };
    Some(match (method, args.len()) {
        ("emptyList", 0) if class == "Collections" => list_of(
            Vec::new(),
            "java.util.Collections$EmptyList",
            Writes::Frozen,
        ),
        ("emptySet", 0) if class == "Collections" => {
            let out = make_set(Vec::new(), SetKind::Linked);
            set_flavor(&out, "java.util.Collections$EmptySet", Writes::Frozen);
            out
        }
        ("emptyMap", 0) if class == "Collections" => {
            let out = gmap(Vec::new());
            set_flavor(&out, "java.util.Collections$EmptyMap", Writes::Frozen);
            out
        }
        ("singletonList", 1) if class == "Collections" => list_of(
            vec![arg0],
            "java.util.Collections$SingletonList",
            Writes::Frozen,
        ),
        ("singleton", 1) if class == "Collections" => {
            let out = make_set(vec![arg0], SetKind::Linked);
            set_flavor(&out, "java.util.Collections$SingletonSet", Writes::Frozen);
            out
        }
        ("singletonMap", 2) if class == "Collections" => {
            let out = gmap(vec![(mapkey::encode(&arg0), args[1].clone())]);
            set_flavor(&out, "java.util.Collections$SingletonMap", Writes::Frozen);
            out
        }
        ("nCopies", 2) if class == "Collections" => {
            let n = as_i64(&arg0)?;
            if n < 0 {
                raise(
                    vm,
                    "IllegalArgumentException",
                    &format!("List length = {n}"),
                );
                return Some(Value::Undef);
            }
            list_of(
                vec![args[1].clone(); n as usize],
                "java.util.Collections$CopiesList",
                Writes::Frozen,
            )
        }
        ("unmodifiableList", 1) if class == "Collections" => wrapped_copy(
            &arg0,
            "java.util.Collections$UnmodifiableRandomAccessList",
            Writes::Frozen,
        )?,
        ("unmodifiableCollection", 1) if class == "Collections" => wrapped_copy(
            &arg0,
            "java.util.Collections$UnmodifiableCollection",
            Writes::Frozen,
        )?,
        ("unmodifiableSet", 1) if class == "Collections" => wrapped_copy(
            &arg0,
            "java.util.Collections$UnmodifiableSet",
            Writes::Frozen,
        )?,
        ("unmodifiableMap", 1) if class == "Collections" => wrapped_copy(
            &arg0,
            "java.util.Collections$UnmodifiableMap",
            Writes::Frozen,
        )?,
        ("synchronizedList", 1) if class == "Collections" => wrapped_copy(
            &arg0,
            "java.util.Collections$SynchronizedRandomAccessList",
            Writes::Open,
        )?,
        ("synchronizedMap", 1) if class == "Collections" => {
            wrapped_copy(&arg0, "java.util.Collections$SynchronizedMap", Writes::Open)?
        }
        ("synchronizedSet", 1) if class == "Collections" => {
            wrapped_copy(&arg0, "java.util.Collections$SynchronizedSet", Writes::Open)?
        }
        ("asList", _) if class == "Arrays" => {
            // `Arrays.asList(array)` spreads an object array; anything else is
            // the varargs list itself.
            let items = match (args.len(), array_elem(&arg0)) {
                (1, Some(ArrayElem::Object | ArrayElem::Ref(_))) => iteration_elements(&arg0),
                _ => args.to_vec(),
            };
            list_of(items, "java.util.Arrays$ArrayList", Writes::FixedSize)
        }
        ("of" | "copyOf", _) if class == "List" => {
            let spread = method == "copyOf" || (args.len() == 1 && array_elem(&arg0).is_some());
            let items = if spread {
                iteration_elements(&arg0)
            } else {
                args.to_vec()
            };
            if items.iter().any(|v| matches!(v, Value::Undef)) {
                raise_opt(vm, "NullPointerException", None);
                return Some(Value::Undef);
            }
            let class = if (1..=2).contains(&items.len()) {
                "java.util.ImmutableCollections$List12"
            } else {
                "java.util.ImmutableCollections$ListN"
            };
            list_of(items, class, Writes::Frozen)
        }
        ("of" | "copyOf", _) if class == "Set" => {
            let items = if method == "copyOf" {
                iteration_elements(&arg0)
            } else {
                args.to_vec()
            };
            if items.iter().any(|v| matches!(v, Value::Undef)) {
                raise_opt(vm, "NullPointerException", None);
                return Some(Value::Undef);
            }
            let mut unique: Vec<Value> = Vec::new();
            for v in items {
                if unique.iter().any(|u| java_equals(u, &v)) {
                    if method == "of" {
                        raise(
                            vm,
                            "IllegalArgumentException",
                            &format!("duplicate element: {}", groovy_str(&v)),
                        );
                        return Some(Value::Undef);
                    }
                    continue;
                }
                unique.push(v);
            }
            let class = if unique.len() <= 2 {
                "java.util.ImmutableCollections$Set12"
            } else {
                "java.util.ImmutableCollections$SetN"
            };
            let out = make_set(unique, SetKind::Linked);
            set_flavor(&out, class, Writes::Frozen);
            out
        }
        ("of", n) if class == "Map" && n % 2 == 0 => {
            let mut entries: Vec<(String, Value)> = Vec::new();
            for pair in args.chunks(2) {
                let key = mapkey::encode(&pair[0]);
                if entries.iter().any(|(k, _)| *k == key) {
                    raise(
                        vm,
                        "IllegalArgumentException",
                        &format!("duplicate key: {}", mapkey::text(&key)),
                    );
                    return Some(Value::Undef);
                }
                entries.push((key, pair[1].clone()));
            }
            let class = if entries.len() == 1 {
                "java.util.ImmutableCollections$Map1"
            } else {
                "java.util.ImmutableCollections$MapN"
            };
            let out = gmap(entries);
            set_flavor(&out, class, Writes::Frozen);
            out
        }
        ("copyOf", 1) if class == "Map" => {
            let entries = as_omap(&arg0)?;
            let class = if entries.len() == 1 {
                "java.util.ImmutableCollections$Map1"
            } else {
                "java.util.ImmutableCollections$MapN"
            };
            let out = gmap(entries);
            set_flavor(&out, class, Writes::Frozen);
            out
        }
        _ => return None,
    })
}

//! `respondsTo` / `hasProperty` on the JDK values groovyrs models, answering
//! the `MetaMethod`s and `MetaProperty`s Apache Groovy's metaclass holds.
//!
//! The answers come from `meta/methods.tsv`, which `meta/gen_methods.groovy`
//! dumps from the oracle itself: for every method name of every modeled JDK
//! class, the list `respondsTo(name)` answers there, in its order, and every
//! property `metaClass.properties` lists. `respondsTo(name, args…)` keeps the
//! methods whose parameters take those arguments.

use super::*;

const TABLE: &str = include_str!("meta/methods.tsv");

/// One `MetaMethod`.
#[derive(Clone)]
pub(super) struct Method {
    name: String,
    is_static: bool,
    owner: String,
    ret: String,
    params: Vec<String>,
    class: String,
    /// `java.lang.reflect.Method.toString` of a reflected method.
    text: Option<String>,
}

/// A `respondsTo` / `hasProperty` answer.
#[derive(Clone)]
pub(super) enum Meta {
    Method(Method),
    Property {
        name: String,
        ty: String,
        class: String,
    },
}

type Index = HashMap<String, (Vec<Method>, Vec<(String, String, String)>)>;

thread_local! {
    /// The table, parsed on first use: receiver class → its methods, in
    /// table order, and its properties.
    static INDEX: RefCell<Option<Index>> = const { RefCell::new(None) };
    /// Set once any `MetaMethod` / `MetaProperty` exists, so the shape tests
    /// cost one flag read without.
    static ANY_META: Cell<bool> = const { Cell::new(false) };
}

fn with_index<R>(f: impl FnOnce(&Index) -> R) -> R {
    INDEX.with(|i| {
        let mut slot = i.borrow_mut();
        let index = slot.get_or_insert_with(parse_table);
        f(index)
    })
}

fn parse_table() -> Index {
    let mut index: Index = HashMap::new();
    for line in TABLE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["M", recv, name, stat, owner, ret, params, class, text] => {
                index.entry(recv.to_string()).or_default().0.push(Method {
                    name: name.to_string(),
                    is_static: *stat == "1",
                    owner: owner.to_string(),
                    ret: ret.to_string(),
                    params: params
                        .split(',')
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect(),
                    class: class.to_string(),
                    text: (*text != "-").then(|| text.to_string()),
                });
            }
            ["P", recv, name, ty, class] => {
                index.entry(recv.to_string()).or_default().1.push((
                    name.to_string(),
                    ty.to_string(),
                    class.to_string(),
                ));
            }
            _ => {}
        }
    }
    index
}

fn wrap(m: Meta) -> Value {
    ANY_META.with(|a| a.set(true));
    heap_push(HeapObj::Meta(Box::new(m)))
}

pub(super) fn as_meta(v: &Value) -> Option<Meta> {
    if !ANY_META.with(Cell::get) {
        return None;
    }
    match v {
        Value::Obj(id) => HEAP.with(|h| match h.borrow().get(*id as usize) {
            Some(HeapObj::Meta(m)) => Some((**m).clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The simple name `param_fit` checks an argument against.
fn simple(ty: &str) -> &str {
    ty.rsplit('.').next().unwrap_or(ty)
}

/// `recv.respondsTo(name, args…)` for a receiver of the JDK class `class`;
/// `None` when the table does not model that class.
pub(super) fn responds_to(class: &str, name: &str, args: &[Value]) -> Option<Value> {
    let found: Vec<Method> = with_index(|index| {
        let (methods, _) = index.get(class)?;
        Some(
            methods
                .iter()
                .filter(|m| m.name == name)
                .filter(|m| {
                    args.is_empty()
                        || (m.params.len() == args.len()
                            && m.params.iter().zip(args).all(|(p, a)| {
                                p == "java.lang.Object" || params::param_fit(simple(p), a).is_some()
                            }))
                })
                .cloned()
                .collect(),
        )
    })?;
    Some(glist(
        found.into_iter().map(|m| wrap(Meta::Method(m))).collect(),
    ))
}

/// `recv.hasProperty(name)` for a receiver of the JDK class `class`: the
/// `MetaProperty`, `null` when there is none, `None` when the class is not
/// modeled.
pub(super) fn has_property(class: &str, name: &str) -> Option<Value> {
    let found = with_index(|index| {
        let (_, props) = index.get(class)?;
        Some(props.iter().find(|(n, _, _)| n == name).cloned())
    })?;
    Some(match found {
        Some((name, ty, class)) => wrap(Meta::Property { name, ty, class }),
        None => Value::Undef,
    })
}

fn class_ref(name: &str) -> Value {
    heap_push(HeapObj::ClassRef(name.to_string()))
}

/// `Class.toString` of a type name: `class X`, `interface X`, or a primitive's
/// bare name.
fn render_class(name: &str) -> String {
    if matches!(
        name,
        "int" | "long" | "short" | "byte" | "double" | "float" | "char" | "boolean" | "void"
    ) {
        name.to_string()
    } else if is_interface_class(name) {
        format!("interface {name}")
    } else {
        format!("class {name}")
    }
}

/// What a `MetaMethod` / `MetaProperty` renders as. A non-reflected method's
/// `toString` is `Object.toString` (class and identity hash) and its fields.
pub(super) fn text(v: &Value) -> Option<String> {
    let Value::Obj(id) = v else { return None };
    Some(match as_meta(v)? {
        Meta::Method(m) => match m.text {
            Some(t) => t,
            None => format!(
                "{}@{:x}[name: {} params: [{}] returns: {} owner: {}]",
                m.class,
                id.wrapping_mul(0x9E37_79B1) | 0x1000_0000,
                m.name,
                m.params
                    .iter()
                    .map(|p| render_class(p))
                    .collect::<Vec<_>>()
                    .join(", "),
                render_class(&m.ret),
                render_class(&m.owner),
            ),
        },
        Meta::Property { class, .. } => {
            format!("{class}@{:x}", id.wrapping_mul(0x9E37_79B1) | 0x1000_0000)
        }
    })
}

pub(super) fn class_name(v: &Value) -> Option<String> {
    Some(match as_meta(v)? {
        Meta::Method(m) => m.class,
        Meta::Property { class, .. } => class,
    })
}

/// The members of a `MetaMethod` / `MetaProperty`; property reads arrive as
/// their getters.
pub(super) fn dispatch(v: &Value, method: &str, args: &[Value]) -> Option<Value> {
    if !args.is_empty() {
        return None;
    }
    Some(match (as_meta(v)?, method) {
        (Meta::Method(m), "getName") => Value::str(m.name),
        (Meta::Method(m), "isStatic") => Value::bool(m.is_static),
        (Meta::Method(m), "getReturnType") => class_ref(&m.ret),
        (Meta::Method(m), "getDeclaringClass") => class_ref(&m.owner),
        (Meta::Method(m), "getParameterTypes" | "getNativeParameterTypes") => {
            glist(m.params.iter().map(|p| class_ref(p)).collect())
        }
        (Meta::Property { name, .. }, "getName") => Value::str(name),
        (Meta::Property { ty, .. }, "getType") => class_ref(&ty),
        (_, "toString") => Value::str(text(v)?),
        _ => return None,
    })
}

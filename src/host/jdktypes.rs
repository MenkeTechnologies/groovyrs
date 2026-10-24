//! The class hierarchy of the JDK and Groovy types groovyrs names.
//!
//! The answers come from `meta/jdk_types.tsv`, which `meta/gen_jdk_types.groovy`
//! generates from the oracle JVM: whether a type is an interface, its direct
//! superclass, and every supertype it has. `Class.isAssignableFrom`, a
//! `case CharSequence:` label against a `Class` subject, and an anonymous
//! class's choice between extending and implementing its named type read it.

use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("meta/jdk_types.tsv");

/// One `jdk_types.tsv` row.
pub(crate) struct JdkType {
    pub(crate) is_interface: bool,
    /// Every supertype, superclasses and interfaces, transitively.
    pub(crate) supers: Vec<&'static str>,
    /// The interfaces it names itself, in `getInterfaces()` order.
    pub(crate) interfaces: Vec<&'static str>,
}

fn table() -> &'static HashMap<&'static str, JdkType> {
    static T: OnceLock<HashMap<&'static str, JdkType>> = OnceLock::new();
    T.get_or_init(|| {
        TABLE
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| {
                let mut cols = l.split('\t');
                let name = cols.next()?;
                let is_interface = cols.next()? == "I";
                let _superclass = cols.next()?;
                let mut list = || match cols.next() {
                    None | Some("-") => Vec::new(),
                    Some(s) => s.split(',').collect(),
                };
                let supers = list();
                let interfaces = list();
                Some((
                    name,
                    JdkType {
                        is_interface,
                        supers,
                        interfaces,
                    },
                ))
            })
            .collect()
    })
}

/// The packages Groovy imports by default, in the order a simple name is
/// looked up in them.
const DEFAULT_PACKAGES: &[&str] = &[
    "java.lang",
    "java.util",
    "java.io",
    "java.net",
    "groovy.lang",
    "groovy.util",
];

/// The qualified name a type written `name` resolves to: itself when it is
/// already a qualified name in the table, else the default-imported type of
/// that simple name (`Map.Entry` is `java.util.Map$Entry`).
pub(crate) fn resolve(name: &str) -> Option<&'static str> {
    let t = table();
    if let Some((k, _)) = t.get_key_value(name) {
        return Some(k);
    }
    if let Some(k) = match name {
        "BigDecimal" => Some("java.math.BigDecimal"),
        "BigInteger" => Some("java.math.BigInteger"),
        _ => None,
    } {
        return Some(k);
    }
    let binary = name.replace('.', "$");
    DEFAULT_PACKAGES.iter().find_map(|pkg| {
        t.get_key_value(format!("{pkg}.{binary}").as_str())
            .map(|(k, _)| *k)
    })
}

/// The table's row for the type `name` resolves to.
pub(crate) fn lookup(name: &str) -> Option<&'static JdkType> {
    table().get(resolve(name)?)
}

/// `target.isAssignableFrom(source)` for two types the table knows; `None`
/// when it knows neither one of them.
pub(crate) fn assignable(target: &str, source: &str) -> Option<bool> {
    let (t, s) = (resolve(target)?, resolve(source)?);
    let row = table().get(s)?;
    Some(t == s || t == "java.lang.Object" || row.supers.contains(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the generator's output, not a hand-trimmed copy: it carries
    /// the types the lookups below depend on, with their whole hierarchy.
    #[test]
    fn table_answers_from_the_whole_hierarchy() {
        assert!(table().len() >= 100, "jdk_types.tsv is truncated");
        assert_eq!(assignable("CharSequence", "java.lang.String"), Some(true));
        assert_eq!(assignable("Iterable", "java.util.TreeSet"), Some(true));
        assert_eq!(
            assignable("java.util.List", "java.util.HashSet"),
            Some(false)
        );
        assert_eq!(resolve("Map.Entry"), Some("java.util.Map$Entry"));
        assert!(lookup("Comparator").is_some_and(|t| t.is_interface));
        assert!(lookup("AbstractList").is_some_and(|t| !t.is_interface));
    }
}

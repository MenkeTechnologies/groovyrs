//! The `groovy --version` banner string.

/// The Groovy language level groovyrs targets — reported by `groovy --version`
/// so tools that parse the version line read a familiar level, followed by the
/// real engine so nothing is misrepresented as Apache Groovy on a JVM.
pub const GROOVY_COMPAT_VERSION: &str = "4.0";

/// The engine name — groovyrs is its own runtime (as HotSpot is for the JVM).
pub const GROOVY_ENGINE: &str = "groovyrs";

/// The `RUNTIME_PLATFORM` string, built from the host arch/OS.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The `groovy --version` banner. Names the targeted language level, then the
/// real engine and its crate version and host triple.
pub fn version_banner() -> String {
    format!(
        "Groovy {} (groovyrs {}) [{}]",
        GROOVY_COMPAT_VERSION,
        env!("CARGO_PKG_VERSION"),
        platform()
    )
}

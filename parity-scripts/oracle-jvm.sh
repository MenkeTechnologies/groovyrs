#!/bin/bash
# Gate: refuse an oracle whose JVM renders doubles by the pre-JDK-19 algorithm.
#
# The `groovy` launcher is a shell script that honours an ambient `JAVA_HOME`
# (`JAVA_HOME="${JAVA_HOME:-…}" exec …`), so which JVM answers depends on the
# caller's environment, not on which `groovy` is on PATH. On this machine the
# same binary resolves three different JVMs:
#
#   JAVA_HOME=<jenv 17>              → JVM 17.0.4.1  → 1.0e23 prints 9.999999999999999E22
#   JAVA_HOME=/opt/homebrew/opt/openjdk@21 → JVM 21.0.12 → 1.0e23 prints 1.0E23
#   JAVA_HOME unset                  → JVM 26.0.2    → 1.0e23 prints 1.0E23
#
# `Double.toString` was reimplemented in JDK 19 (JDK-4511638) to emit the
# shortest round-tripping decimal, so a JDK 17 oracle disagrees with every
# JDK 19+ one — and with groovyrs, whose `decimal::format_double` implements the
# JDK 19+ rule. A run against it reports spurious divergences on every double,
# or freezes a snapshot no current JVM would reproduce.
#
# Usage:  oracle_jvm_gate "<oracle command>" "<caller name for messages>"
# Exits 2 (fails closed) on a stale JVM, on a `groovy` that will not run, and on
# a probe whose output it cannot recognise at all.

oracle_jvm_gate() {
  local oracle="$1" who="${2:-parity}"
  local ver probe tmp
  ver="$("$oracle" --version 2>&1 | head -1)"
  tmp="$(mktemp -d)"
  # Two probes, so a JVM that somehow answers one by accident still has to
  # answer the other: the JDK-19 rendering change and a denormal, whose
  # two-significant-digit rule the same rewrite introduced.
  printf 'println 1.0e23d\nprintln Double.MIN_VALUE\n' > "$tmp/probe.groovy"
  probe="$(timeout 120 "$oracle" "$tmp/probe.groovy" 2>&1)"
  rm -rf "$tmp"
  echo "$who: oracle $oracle — $ver"
  echo "$who: JAVA_HOME=${JAVA_HOME:-<unset>}"
  case "$probe" in
    *'1.0E23'*'4.9E-324'*) return 0 ;;
  esac
  echo "$who: REFUSING this oracle — its JVM renders doubles by the pre-JDK-19 algorithm."
  echo "$who:   $ver"
  echo "$who:   JAVA_HOME=${JAVA_HOME:-<unset>}"
  echo "$who:   probe output: $(printf '%s' "$probe" | tr '\n' '|')"
  echo "$who:   expected 1.0E23 and 4.9E-324 (JDK 19+); JDK 17 gives 9.999999999999999E22."
  echo "$who: re-run with JAVA_HOME=/opt/homebrew/opt/openjdk@21, or with JAVA_HOME unset."
  exit 2
}

#!/bin/bash
# Gate: refuse an oracle whose JVM renders doubles by the pre-JDK-19 algorithm,
# or whose default locale would change what the reference prints.
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
  # Four probes. Two for the JVM: a JVM that somehow answers one by accident
  # still has to answer the other — the JDK-19 rendering change, and a denormal,
  # whose two-significant-digit rule the same rewrite introduced.
  #
  # Two for the LOCALE, which is a second contamination axis the version check
  # cannot see. The JVM reads `user.language`/`user.country` from the ambient
  # environment (and honours `JAVA_OPTS`), and two frozen behaviours move with
  # it: number formatting (`String.format("%,.2f", 1234.5)` is `1,234.50` under
  # en-US and `1.234,50` under de-DE) and case mapping (`"hi".toUpperCase()` is
  # `HI` under en-US and `Hİ` under tr-TR, because Turkish maps a dotted i).
  #
  # `examples/GStrings.groovy` prints `greeting.toUpperCase()` where the
  # greeting is `hi $name`, so the frozen `tests/data/parity_expected.txt`
  # really does depend on this: regenerating it under `JAVA_OPTS=-Duser.language=tr`
  # would pin `Hİ WORLD` and every later run on an en-US machine would report a
  # groovyrs divergence that is nothing of the kind. groovyrs has no locale —
  # its case mapping and number formatting are the en-US answers — so an oracle
  # that is not en-US-equivalent is measuring the wrong thing.
  printf 'println 1.0e23d\nprintln Double.MIN_VALUE\nprintln String.format("%%,.2f", 1234.5)\nprintln "hi".toUpperCase()\n' > "$tmp/probe.groovy"
  probe="$(timeout 120 "$oracle" "$tmp/probe.groovy" 2>&1)"
  rm -rf "$tmp"
  echo "$who: oracle $oracle — $ver"
  echo "$who: JAVA_HOME=${JAVA_HOME:-<unset>}"

  refuse() {
    echo "$who: REFUSING this oracle — $1"
    echo "$who:   $ver"
    echo "$who:   JAVA_HOME=${JAVA_HOME:-<unset>}"
    echo "$who:   JAVA_OPTS=${JAVA_OPTS:-<unset>}  LANG=${LANG:-<unset>}"
    echo "$who:   probe output: $(printf '%s' "$probe" | tr '\n' '|')"
    echo "$who:   $2"
    exit 2
  }

  case "$probe" in
    *'1.0E23'*'4.9E-324'*) ;;
    *) refuse "its JVM renders doubles by the pre-JDK-19 algorithm." \
         "expected 1.0E23 and 4.9E-324 (JDK 19+); JDK 17 gives 9.999999999999999E22. Re-run with JAVA_HOME=/opt/homebrew/opt/openjdk@21, or with JAVA_HOME unset." ;;
  esac
  case "$probe" in
    *'1,234.50'*) ;;
    *) refuse "its JVM's default locale does not format numbers the en-US way." \
         "expected 1,234.50 from String.format(\"%,.2f\", 1234.5). Clear user.language/user.country from JAVA_OPTS, or re-run with JAVA_OPTS='-Duser.language=en -Duser.country=US'." ;;
  esac
  case "$probe" in
    *'HI'*) ;;
    *) refuse "its JVM's default locale does not case-map the en-US way." \
         "expected HI from \"hi\".toUpperCase(); a Turkish locale gives Hİ. Re-run with JAVA_OPTS='-Duser.language=en -Duser.country=US'." ;;
  esac
  return 0
}

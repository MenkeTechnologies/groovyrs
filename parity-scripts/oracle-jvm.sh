#!/bin/bash
# Resolve and PIN the oracle: an absolute `groovy` launcher plus a JVM whose
# rendering the frozen expectations were captured under. Refuse anything else.
#
# The `groovy` launcher is a shell script that honours an ambient `JAVA_HOME`
# (`JAVA_HOME="${JAVA_HOME:-…}" exec …`), so which JVM answers depends on the
# caller's environment, not on which `groovy` is on PATH. On this machine the
# same binary resolves three different JVMs:
#
#   JAVA_HOME=<jenv 17>                    → JVM 17.0.4.1 → 1.0e23 prints 9.999999999999999E22
#   JAVA_HOME=/opt/homebrew/opt/openjdk@21 → JVM 21.0.12  → 1.0e23 prints 1.0E23
#   JAVA_HOME unset                        → JVM 26.0.2   → 1.0e23 prints 1.0E23
#
# `Double.toString` was reimplemented in JDK 19 (JDK-4511638 / JDK-8291240) to
# emit the shortest round-tripping decimal, so a JDK 17 oracle disagrees with
# every JDK 19+ one — and with groovyrs, whose `decimal::format_double`
# implements the JDK 19+ rule. A run against it reports spurious divergences on
# every double, or freezes a snapshot no current JVM would reproduce.
#
# A gate that only refused left the caller to fix their environment by hand, and
# an interactive shell on this machine exports the jenv 17 home, so the usual answer
# was "re-run with a different JAVA_HOME". The gate now SEARCHES: it probes a
# list of candidate JVM homes and pins the first conforming one for the rest of
# the process, printing exactly what it selected. It still fails closed — if no
# candidate renders the JDK 19+ way under an en-US locale, it exits 2 naming
# every candidate it tried and what that candidate printed.
#
# Usage:  oracle_jvm_gate "<oracle command>" "<caller name for messages>"
# Sets:   ORACLE_ABS  — the absolute launcher path the caller must invoke
#         JAVA_HOME   — exported (or unset) to the pinned JVM
# Env:    GROOVYRS_PARITY_JAVA_HOME  — pin this home and try no other
# Exits 2 (fails closed) on a `groovy` that will not run and on a candidate list
# that contains no conforming JVM.

# The four probes the gate keys on.
#
# Two for the JVM: a JVM that somehow answers one by accident still has to
# answer the other — the JDK-19 rendering change, and a denormal, whose
# two-significant-digit rule the same rewrite introduced.
#
# Two for the LOCALE, which is a second contamination axis the version check
# cannot see. The JVM reads `user.language`/`user.country` from the ambient
# environment (and honours `JAVA_OPTS`), and two frozen behaviours move with it:
# number formatting (`String.format("%,.2f", 1234.5)` is `1,234.50` under en-US
# and `1.234,50` under de-DE) and case mapping (`"hi".toUpperCase()` is `HI`
# under en-US and `Hİ` under tr-TR, because Turkish maps a dotted i).
#
# `examples/GStrings.groovy` prints `greeting.toUpperCase()` where the greeting
# is `hi $name`, so the frozen `tests/data/parity_expected.txt` really does
# depend on this: regenerating it under `JAVA_OPTS=-Duser.language=tr` would pin
# `Hİ WORLD` and every later run on an en-US machine would report a groovyrs
# divergence that is nothing of the kind. groovyrs has no locale — its case
# mapping and number formatting are the en-US answers — so an oracle that is not
# en-US-equivalent is measuring the wrong thing.
_ORACLE_PROBE_SRC='println 1.0e23d
println Double.MIN_VALUE
println String.format("%,.2f", 1234.5)
println "hi".toUpperCase()'

# Run the probe under one candidate home. $1 = absolute oracle, $2 = home or the
# literal `<unset>`. Echoes the probe's stdout+stderr with newlines folded to `|`.
_oracle_probe() {
  local oracle="$1" home="$2" tmp out
  tmp="$(mktemp -d)"
  printf '%s\n' "$_ORACLE_PROBE_SRC" > "$tmp/probe.groovy"
  if [ "$home" = "<unset>" ]; then
    out="$(env -u JAVA_HOME timeout 120 "$oracle" "$tmp/probe.groovy" 2>&1)"
  else
    out="$(env JAVA_HOME="$home" timeout 120 "$oracle" "$tmp/probe.groovy" 2>&1)"
  fi
  rm -rf "$tmp"
  printf '%s' "$out" | tr '\n' '|'
}

# The Groovy/JVM banner under one candidate home, folded to a single line.
_oracle_version() {
  local oracle="$1" home="$2"
  if [ "$home" = "<unset>" ]; then
    env -u JAVA_HOME "$oracle" --version 2>&1 | head -1
  else
    env JAVA_HOME="$home" "$oracle" --version 2>&1 | head -1
  fi
}

# Does this probe output describe a JDK 19+ JVM under an en-US locale?
# Returns 0 on conformance; otherwise echoes the reason and returns 1.
_oracle_conforms() {
  case "$1" in
    *'1.0E23'*'4.9E-324'*) ;;
    *) echo "renders doubles by the pre-JDK-19 algorithm (want 1.0E23 and 4.9E-324)"; return 1 ;;
  esac
  case "$1" in
    *'1,234.50'*) ;;
    *) echo "default locale does not format numbers the en-US way (want 1,234.50)"; return 1 ;;
  esac
  case "$1" in
    *'HI'*) ;;
    *) echo "default locale does not case-map the en-US way (want HI, a Turkish locale gives Hİ)"; return 1 ;;
  esac
  return 0
}

oracle_jvm_gate() {
  local oracle="$1" who="${2:-parity}"
  local abs cand home probe why

  abs="$(command -v "$oracle" 2>/dev/null)"
  if [ -z "$abs" ]; then
    echo "$who: no reference '$oracle' on PATH"; exit 2
  fi
  # An absolute path is what the caller runs from here on: `command -v` is
  # evaluated once, in this environment, so a later PATH change (or a `groovy`
  # function shadowing the launcher) cannot swap the oracle out mid-run.
  case "$abs" in /*) ;; *) abs="$(cd "$(dirname "$abs")" && pwd)/$(basename "$abs")" ;; esac
  ORACLE_ABS="$abs"

  # Candidate JVM homes, in preference order. An explicit
  # GROOVYRS_PARITY_JAVA_HOME is honoured ALONE: a caller who names a JVM wants
  # that JVM measured or the run refused, never a silent substitution.
  local -a cands=()
  if [ -n "${GROOVYRS_PARITY_JAVA_HOME:-}" ]; then
    cands=("$GROOVYRS_PARITY_JAVA_HOME")
  else
    # The ambient home first — an environment already pinned to a conforming JVM
    # is left exactly as the caller set it.
    [ -n "${JAVA_HOME:-}" ] && cands+=("$JAVA_HOME")
    cands+=("<unset>")
    local h
    for h in /opt/homebrew/opt/openjdk /opt/homebrew/opt/openjdk@25 \
             /opt/homebrew/opt/openjdk@23 /opt/homebrew/opt/openjdk@21 \
             /usr/local/opt/openjdk /usr/lib/jvm/default-java; do
      [ -d "$h" ] && cands+=("$h")
    done
    if [ -x /usr/libexec/java_home ]; then
      for v in 25 23 21 19; do
        h="$(/usr/libexec/java_home -v "$v" 2>/dev/null)" && [ -n "$h" ] && cands+=("$h")
      done
    fi
  fi

  local -a tried=()
  for cand in "${cands[@]}"; do
    probe="$(_oracle_probe "$abs" "$cand")"
    if why="$(_oracle_conforms "$probe")"; then
      # PIN it: every later invocation in this process inherits the same JVM.
      if [ "$cand" = "<unset>" ]; then unset JAVA_HOME; else export JAVA_HOME="$cand"; fi
      # Self-report. Which JVM answered is the single fact that decides whether
      # a frozen double expectation means anything, so it is printed on every
      # run, not only on a refusal.
      echo "$who: oracle    $abs"
      echo "$who: banner    $(_oracle_version "$abs" "$cand")"
      echo "$who: JAVA_HOME ${JAVA_HOME:-<unset>}  (pinned)"
      echo "$who: probe     $probe"
      return 0
    fi
    tried+=("$cand — $why | probe: $probe")
  done

  echo "$who: REFUSING every candidate JVM — no conforming oracle."
  echo "$who:   oracle: $abs"
  echo "$who:   JAVA_OPTS=${JAVA_OPTS:-<unset>}  LANG=${LANG:-<unset>}"
  local t
  for t in "${tried[@]}"; do echo "$who:   tried $t"; done
  echo "$who:   want a JDK 19+ JVM (JDK-4511638 shortest-round-trip Double.toString)"
  echo "$who:   under an en-US locale; set GROOVYRS_PARITY_JAVA_HOME to one."
  exit 2
}

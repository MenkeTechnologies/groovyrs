#!/bin/bash
# Differential probe harness: diff many small Groovy snippets ("probes") against
# the reference `groovy` one at a time, but pay the JVM's ~1s start-up cost only
# ONCE by batching every probe into a single oracle program.
#
# Each probe is wrapped in `try { … } catch (Throwable t) { println 'EXC:' + … }`
# so a throw is a comparable observation rather than a dead run, and is preceded
# by a `##P<n>` marker line the splitter keys on. The oracle runs the whole
# batch; groovyrs runs each probe on its own (it starts in milliseconds), so one
# probe that fails to parse or aborts cannot swallow the probes after it.
#
#   Usage: bash parity-scripts/fuzz.sh [probes-file] [-v]
#          GROOVYRS_PARITY_GROOVY=/path/to/groovy  overrides the oracle
#          GROOVYRS_PARITY_OURS=/path/to/groovy    overrides the build under test
#
# Probe file format: probe bodies separated by a line containing only `%%`.
# Lines starting with `#` at the very top of a probe body are comments/titles.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# `GROOVYRS_PARITY_OURS` points the harness at another build — the way to run a
# probe set through a pre-change binary and check it actually fails there.
OURS="${GROOVYRS_PARITY_OURS:-$ROOT/target/debug/groovy}"
PROBES="${1:-$ROOT/parity-scripts/probes.txt}"
[ "${1:-}" = "-v" ] && { PROBES="$ROOT/parity-scripts/probes.txt"; VERBOSE=-v; } || VERBOSE="${2:-}"
ORACLE="${GROOVYRS_PARITY_GROOVY:-groovy}"

command -v "$ORACLE" >/dev/null || { echo "fuzz: no reference '$ORACLE' on PATH"; exit 2; }
[ -x "$OURS" ] || { echo "fuzz: $OURS not built (cargo build)"; exit 2; }
[ -f "$PROBES" ] || { echo "fuzz: no probe file $PROBES"; exit 2; }

# The `groovy` launcher resolves its JVM from an ambient `JAVA_HOME`, and a
# pre-JDK-19 one renders every double differently. Refuse it rather than
# reporting its disagreements as groovyrs divergences.
. "$ROOT/parity-scripts/oracle-jvm.sh"
oracle_jvm_gate "$ORACLE" fuzz

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Split the probe file into $TMP/p<N>.body, one probe body per file.
command perl -e '
  my $f = shift; my $dir = shift;
  open my $fh, "<", $f or die $!;
  local $/; my $all = <$fh>;
  my @p = split /^%%$/m, $all;
  my $n = 0;
  for my $b (@p) {
    $b =~ s/\A\s+//;
    $b =~ s/\A(?:#[^\n]*\n)+//;   # drop the probe-file header/title comments
    $b =~ s/\A\s+//; $b =~ s/\s+\z//;
    next unless length $b;
    open my $o, ">", "$dir/p$n.body" or die $!;
    print $o $b, "\n"; close $o; $n++;
  }
  print "$n\n";
' "$PROBES" "$TMP" > "$TMP/count"
N="$(cat "$TMP/count")"
echo "fuzz: $N probes"

if [ "$N" -eq 0 ]; then
  echo "fuzz: probe file $PROBES yielded 0 probes — nothing to compare"; exit 2
fi

# Write the file each probe is *run from* — the same bytes on both sides.
for ((i=0; i<N; i++)); do
  {
    printf 'try {\n'
    cat "$TMP/p$i.body"
    printf '\n} catch (Throwable t) { println "EXC:" + t.getClass().getName() }\n'
  } > "$TMP/p$i.groovy"
done

# The oracle runs each probe in its OWN script, from one JVM.
#
# It used to concatenate the probes into a handful of batched programs. That
# amortised the JVM's ~3.5s start-up (854 standalone runs is ~50 minutes) but it
# put every probe in one script `run()` method, sharing one `Binding` — so an
# undeclared assignment leaked forward. `counter = 0` in one probe and
# `counter += 1` in a later one printed `2` batched and raised
# `MissingPropertyException` standalone, and groovyrs, which runs each probe on
# its own, was being compared against the wrong answer. No probe in the corpus
# depended on it, so the hazard was latent rather than active — but "no probe
# does today" is not a property the harness enforces, and a probe added later
# would have been silently mismeasured.
#
# A driver that hands each probe to its own `GroovyShell` fixes it without
# giving up the amortisation: a fresh `GroovyShell` carries a fresh `Binding`,
# so `counter += 1` raises exactly as it does standalone. `parse(text, name)`
# additionally gives the script the class name a standalone run would derive
# from the filename, so `this.getClass().getName()` answers `p7` either way, and
# the `Binding` is seeded with the empty `args` the launcher supplies.
#
# The driver reads the probe files at run time rather than embedding them, so
# its own `run()` is a constant handful of bytecode — the 64KB method limit that
# forced the chunking is gone, and there is no chunk boundary left to tune.
#
# What one JVM still shares is process-wide state: system properties, the
# default locale, static initialisers. That is unreachable from this corpus and
# is the residual difference from 854 separate processes.
cat > "$TMP/driver.groovy" <<'DRIVER'
def dir = new File(args[0])
def n = args[1] as int
for (int i = 0; i < n; i++) {
  println "##P$i"
  def name = "p${i}.groovy"
  try {
    def shell = new GroovyShell(new Binding([args: new String[0]]))
    shell.parse(new File(dir, name).getText("UTF-8"), name).run()
  } catch (Throwable t) {
    println "EXC:" + t.getClass().getName()
  }
}
DRIVER
timeout 900 "$ORACLE" "$TMP/driver.groovy" "$TMP" "$N" > "$TMP/oracle.out" 2>"$TMP/oracle.err"
orc=$?
# A non-zero oracle rc is fatal even when it left partial output behind. Every
# probe the run never reached would otherwise become a SKIP, and a run that
# skipped everything used to still exit 0 — a truncated oracle reading as a
# clean sweep. `timeout` (rc 124) is the case that actually bit us.
if [ $orc -ne 0 ]; then
  echo "fuzz: oracle driver failed (rc=$orc) — the corpus is unmeasured:"
  head -20 "$TMP/oracle.err"; exit 2
fi

# Split the oracle's stdout back out into per-probe expected files.
command perl -e '
  my $f = shift; my $dir = shift;
  open my $fh, "<", $f or die $!;
  my ($cur, $out);
  while (my $l = <$fh>) {
    if ($l =~ /^##P(\d+)$/) { close $out if $out; $cur = $1; open $out, ">", "$dir/p$cur.exp" or die $!; next; }
    print $out $l if $out;
  }
  close $out if $out;
' "$TMP/oracle.out" "$TMP"

# The driver has to have reached every probe. One marker per probe is the only
# evidence of that; a short count means the oracle died partway and the rest of
# the corpus was never run, so the comparison below would measure a truncated
# set and report it as a full sweep.
markers=$(grep -c '^##P' "$TMP/oracle.out")
if [ "$markers" -ne "$N" ]; then
  echo "fuzz: oracle emitted $markers/$N probe markers — run truncated, corpus unmeasured"
  exit 2
fi

# A probe only counts as a comparison if the ORACLE ran it: it has to have
# emitted its `##P` marker (so the driver reached it) and then printed something.
# A probe the reference never executed measures nothing — our side failing too
# would read as agreement — so those are reported as SKIPPED, never as passes.
# A run of ours that has to be killed is a MISS whatever the oracle printed;
# `timeout` exits 124, which an empty-vs-empty `cmp` would otherwise call equal.
pass=0; fail=0; skip=0
declare -a misses
declare -a skips
for ((i=0; i<N; i++)); do
  if [ ! -f "$TMP/p$i.exp" ] || [ ! -s "$TMP/p$i.exp" ]; then
    skip=$((skip+1)); skips+=("$i")
    echo "──── SKIP $i (oracle produced no output — not a comparison) ────"
    head -3 "$TMP/p$i.body"
    continue
  fi
  # `$TMP/p$i.groovy` is the same file the oracle's driver parsed, byte for byte
  # and under the same script name.
  timeout 20 "$OURS" "$TMP/p$i.groovy" > "$TMP/p$i.got" 2>"$TMP/p$i.err"
  rc=$?
  if [ $rc -ne 124 ] && cmp -s "$TMP/p$i.exp" "$TMP/p$i.got"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); misses+=("$i")
    echo "──── PROBE $i ────"
    head -3 "$TMP/p$i.body"
    echo "  groovy : $(command perl -pe 's/\n/ | /' < "$TMP/p$i.exp")"
    if [ $rc -eq 124 ]; then
      echo "  groovyrs: TIMED OUT after 20s (killed)"
    else
      echo "  groovyrs: $(command perl -pe 's/\n/ | /' < "$TMP/p$i.got")"
    fi
    if [ -s "$TMP/p$i.err" ]; then echo "  stderr : $(head -2 "$TMP/p$i.err" | command perl -pe 's/\n/ | /')"; fi
  fi
done

compared=$((pass+fail))
echo ""
echo "════════════════════════════════════════════"
echo "PROBE PARITY: $pass / $compared match  (oracle: $ORACLE)"
echo "  $N probes, $skip skipped (oracle never ran them)"
echo "════════════════════════════════════════════"
# A run that compared nothing is not a pass. `fail -eq 0` alone is true of a
# sweep where every probe skipped, which is how a wiped or unreachable corpus
# reads as success; the comparison count has to reach the exit status too.
if [ $compared -eq 0 ]; then
  echo "fuzz: 0 probes compared — nothing was measured"
  exit 2
fi
[ $fail -eq 0 ]

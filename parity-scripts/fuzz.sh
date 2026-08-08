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
#
# Probe file format: probe bodies separated by a line containing only `%%`.
# Lines starting with `#` at the very top of a probe body are comments/titles.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OURS="$ROOT/target/debug/groovy"
PROBES="${1:-$ROOT/parity-scripts/probes.txt}"
[ "${1:-}" = "-v" ] && { PROBES="$ROOT/parity-scripts/probes.txt"; VERBOSE=-v; } || VERBOSE="${2:-}"
ORACLE="${GROOVYRS_PARITY_GROOVY:-groovy}"

command -v "$ORACLE" >/dev/null || { echo "fuzz: no reference '$ORACLE' on PATH"; exit 2; }
[ -x "$OURS" ] || { echo "fuzz: $OURS not built (cargo build)"; exit 2; }
[ -f "$PROBES" ] || { echo "fuzz: no probe file $PROBES"; exit 2; }

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

# One batched oracle program: marker, probe, marker, probe, …
: > "$TMP/all.groovy"
for ((i=0; i<N; i++)); do
  {
    printf 'println "##P%d"\ntry {\n' "$i"
    cat "$TMP/p$i.body"
    printf '\n} catch (Throwable t) { println "EXC:" + t.getClass().getName() }\n'
  } >> "$TMP/all.groovy"
done

timeout 300 "$ORACLE" "$TMP/all.groovy" > "$TMP/oracle.out" 2>"$TMP/oracle.err"
orc=$?
if [ $orc -ne 0 ] && [ ! -s "$TMP/oracle.out" ]; then
  echo "fuzz: oracle batch failed (rc=$orc):"; head -20 "$TMP/oracle.err"; exit 2
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

pass=0; fail=0
declare -a misses
for ((i=0; i<N; i++)); do
  [ -f "$TMP/p$i.exp" ] || : > "$TMP/p$i.exp"
  {
    printf 'try {\n'
    cat "$TMP/p$i.body"
    printf '\n} catch (Throwable t) { println "EXC:" + t.getClass().getName() }\n'
  } > "$TMP/p$i.groovy"
  timeout 20 "$OURS" "$TMP/p$i.groovy" > "$TMP/p$i.got" 2>"$TMP/p$i.err"
  if cmp -s "$TMP/p$i.exp" "$TMP/p$i.got"; then
    pass=$((pass+1))
  else
    fail=$((fail+1)); misses+=("$i")
    echo "──── PROBE $i ────"
    head -3 "$TMP/p$i.body"
    echo "  groovy : $(command perl -pe 's/\n/ | /' < "$TMP/p$i.exp")"
    echo "  groovyrs: $(command perl -pe 's/\n/ | /' < "$TMP/p$i.got")"
    if [ -s "$TMP/p$i.err" ]; then echo "  stderr : $(head -2 "$TMP/p$i.err" | command perl -pe 's/\n/ | /')"; fi
  fi
done

echo ""
echo "════════════════════════════════════════════"
echo "PROBE PARITY: $pass / $N match  (oracle: $ORACLE)"
echo "════════════════════════════════════════════"
[ $fail -eq 0 ]

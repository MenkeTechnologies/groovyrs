#!/bin/sh
# Regenerates data/tzdb.bin, the tz database groovyrs embeds (src/host/tz.rs).
#
# The JDK ships its own tz database instead of reading the host's, so a zone
# answers the same on every machine; groovyrs does the same. This script
# downloads one IANA release (code and data), builds that release's zic,
# compiles every zone the JDK knows (all regions plus `backward`; not
# `factory`) as slim TZif, and packs them into one file:
#
#   "GTZ1"  u32 entry count
#   per entry: u16 name length, name (UTF-8), u32 data length, TZif bytes
#
# all integers big-endian, entries sorted by name.
#
# Usage: scripts/gen_tzdb.sh [release]   (default: the release in data/tzdb.version)
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
release=${1:-$(cat "$root/data/tzdb.version")}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$work"
curl -fsSL -o tzcode.tgz "https://data.iana.org/time-zones/releases/tzcode$release.tar.gz"
curl -fsSL -o tzdata.tgz "https://data.iana.org/time-zones/releases/tzdata$release.tar.gz"
tar xzf tzcode.tgz
tar xzf tzdata.tgz
make -s version.h tzdir.h
cc -O2 -DHAVE_GETTEXT=0 -o zic zic.c

mkdir out
./zic -b slim -d out \
    africa antarctica asia australasia europe northamerica southamerica \
    etcetera backward

perl -e '
    use strict;
    use warnings;
    my ($dir, $dest) = @ARGV;
    my @names;
    my @todo = ("");
    while (defined(my $rel = shift @todo)) {
        opendir my $dh, "$dir/$rel" or die "$dir/$rel: $!";
        for my $e (sort grep { !/^\./ } readdir $dh) {
            my $name = $rel eq "" ? $e : "$rel/$e";
            if (-d "$dir/$name") { push @todo, $name } else { push @names, $name }
        }
    }
    @names = sort @names;
    open my $out, ">:raw", $dest or die "$dest: $!";
    print $out "GTZ1", pack("N", scalar @names);
    for my $name (@names) {
        open my $in, "<:raw", "$dir/$name" or die "$name: $!";
        local $/;
        my $data = <$in>;
        print $out pack("n", length $name), $name, pack("N", length $data), $data;
    }
    close $out or die;
    printf "%d zones, %d bytes\n", scalar @names, -s $dest;
' out "$root/data/tzdb.bin"

echo "$release" > "$root/data/tzdb.version"

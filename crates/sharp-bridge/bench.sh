#!/bin/sh
# Counts the instructions the front end spends on a generated class of 10,000 and of 20,000 methods, as `.sharp`
# through `sharp_lower` and as `.php` through the checker, and the instructions PHP's own compile spends on the `.php`
# class through `php -l`. Each count is the "instructions retired" of `/usr/bin/time -l` on macOS, three runs each.
# The first lines count each process on an empty file, the startup every later count includes. The `.sharp` class is
# also counted stopped after parsing, after binding names and after the semantic checks, so each difference is one
# pass, and `sharp_lower` adds the lowering.
#
# Usage: crates/sharp-bridge/bench.sh [methods...]

set -eu

cd "$(dirname "$0")/../.."
rustup run 1.97.0 cargo build --quiet --profile memory-debug -p mago-sharp-bridge --example front_end

front_end=target/memory-debug/examples/front_end
classes=$(mktemp -d)
trap 'rm -rf "$classes"' EXIT

instructions() {
    for run in 1 2 3; do
        /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ { printf " %.3fe9", $1 / 1e9 }'
    done
}

: > "$classes/Empty.sharp"
echo "<?php" > "$classes/Empty.php"
echo "empty file, the startup of each process:"
echo "  sharp_lower .sharp:$(instructions "$front_end" "$classes/Empty.sharp")"
echo "  checker .php:      $(instructions "$front_end" "$classes/Empty.php")"
echo "  php -l .php:       $(instructions php -l "$classes/Empty.php")"

[ $# -gt 0 ] || set -- 10000 20000
for methods in "$@"; do
    awk -v methods="$methods" 'BEGIN {
        print "namespace App;\n\nclass Big\n{"
        for (i = 0; i < methods; i++) printf "    public int m%d(int value)\n    {\n        return value + %d;\n    }\n\n", i, i
        print "}"
    }' > "$classes/Big$methods.sharp"
    awk -v methods="$methods" 'BEGIN {
        print "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nclass Big\n{"
        for (i = 0; i < methods; i++) printf "    public function m%d(int $value): int\n    {\n        return $value + %d;\n    }\n\n", i, i
        print "}"
    }' > "$classes/Big$methods.php"

    echo "$methods methods, $(wc -c < "$classes/Big$methods.sharp" | tr -d ' ') bytes of PHP#:"
    echo "  parse .sharp:      $(instructions "$front_end" --until parse "$classes/Big$methods.sharp")"
    echo "  + names .sharp:    $(instructions "$front_end" --until names "$classes/Big$methods.sharp")"
    echo "  + checks .sharp:   $(instructions "$front_end" --until checks "$classes/Big$methods.sharp")"
    echo "  sharp_lower .sharp:$(instructions "$front_end" "$classes/Big$methods.sharp")"
    echo "  checker .php:      $(instructions "$front_end" "$classes/Big$methods.php")"
    echo "  php -l .php:       $(instructions php -l "$classes/Big$methods.php")"
done

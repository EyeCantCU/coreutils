#!/bin/bash -e
ver="9.12"
repo=https://github.com/coreutils/coreutils
curl -L "${repo}/releases/download/v${ver}/coreutils-${ver}.tar.xz" | tar --strip-components=1 -xJf -

# TODO stop backporting tests from master at GNU coreutils > $ver
backport=(
  # https://github.com/coreutils/coreutils/issues/355
  env/env.sh
  env/printenv.sh
)
for f in "${backport[@]}"
  do curl -L ${repo}/raw/refs/heads/master/tests/$f > tests/$f
done

# A test that does not exist in $ver at all is absent from its test list, so
# `make check` would silently never run it.  Register those in both the automake
# input and the generated Makefile.in: configure derives Makefile from the
# latter, and build-gnu.sh deliberately keeps automake from re-running.
for f in "${backport[@]}"; do
  grep -qF "tests/$f" tests/local.mk ||
    sed -i "s|^all_tests =.*|&\n  tests/$f\t\t\t\t\\\\|" tests/local.mk
  grep -qF "tests/$f" Makefile.in ||
    sed -i "s|^all_tests =.*|&\n  tests/$f\t\t\t\t\\\\|" Makefile.in
done

# Tests from $ver that a backport above replaces and that contradict it.  Drop
# them from the tree and from both test lists so they are neither run nor
# required by make.
superseded=(
  # The backported env/printenv.sh replaces it; the two expect opposite
  # behaviour for QUOTING_STYLE on redirected output.
  misc/printenv.sh
)
for f in "${superseded[@]}"; do
  rm -f "tests/$f"
  sed -i "\|^[[:space:]]*tests/${f}[[:space:]]*\\\\\$|d" tests/local.mk Makefile.in
  # Fail loudly if the list format changed and an entry survived the deletion.
  if grep -F "tests/${f}" tests/local.mk Makefile.in; then
    echo "fetch-gnu.sh: could not remove tests/${f} from the test lists" >&2
    exit 1
  fi
done
